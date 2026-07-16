//! 文件职责：维护日志搜索窗口、后台搜索任务、来源树多选和结果跳转逻辑。
//! 创建日期：2026-06-11
//! 修改日期：2026-07-16
//! 作者：Argus 开发团队
//! 主要功能：把真实日志搜索能力接入 ArgusApp，同时保持搜索 UI 状态与日志读取状态解耦。

use std::borrow::Borrow;
use std::collections::BTreeSet;
use std::ops::Range;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;

use gpui::{
    AppContext, ClipboardItem, Context, Keystroke, Modifiers, Pixels, ScrollStrategy, Timer,
    WindowBounds, WindowOptions, px, size,
};

use super::{
    ArgusApp, LogSearchInputKind, QuickMatchKey, SEARCH_RESULT_PANEL_HEIGHT_MIN, SearchResultGroup,
    SearchResultListItem, SearchResultPanelResizeDrag, SearchRunKind, TabKind, TextInputState,
};
use crate::app::LOG_VIEWER_ROW_HEIGHT;
use crate::config::SEARCH_RECENT_KEYWORDS_MAX;
use crate::infra::text_selection::{
    TextSelectionGranularity, character_count, insert_text_at_character_index,
    remove_character_range, slice_character_range,
};
use crate::loader::{LoadReport, LogSourceLoader, SourceId, SourceKind, SourceRegistry};
use crate::search::search_engine::{
    CurrentLogMatchCount, CurrentLogMatchDirection, CurrentLogMatchNavigation,
    CurrentLogMatchPosition, SearchEngine, SearchProgress, SearchQuery, SearchRequest,
    SearchResult, SearchScope, SearchTarget, SearchTaskSummary,
};
use crate::search::search_task::SearchTaskState;
use crate::ui::log_search_window::LogSearchWindow;

/// 搜索窗口默认宽度。
const LOG_SEARCH_WINDOW_WIDTH: f32 = 560.0;
/// 搜索窗口默认高度。
///
/// 内容（标题 + 关键字 + 目录 + 模式行 + 操作行 + 内外边距）约需 244px；底部用 flex_1 占位
/// 把操作行顶到底部，280px 既容下完整内容与按钮上方留白，也为关键字历史下拉留出展开空间。
const LOG_SEARCH_WINDOW_HEIGHT: f32 = 280.0;
/// 搜索窗口最小宽度。
const LOG_SEARCH_WINDOW_MIN_WIDTH: f32 = 460.0;
/// 搜索窗口最小高度。
const LOG_SEARCH_WINDOW_MIN_HEIGHT: f32 = 250.0;
/// 搜索后台事件合并刷新间隔；降低大量结果时主窗口重绘频率。
const LOG_SEARCH_UI_POLL_INTERVAL_MS: u64 = 80;
/// 单次 UI tick 最多处理的后台事件数，避免一次性追加过多结果造成滚动卡顿。
const LOG_SEARCH_MAX_EVENTS_PER_TICK: usize = 32;
/// 搜索结果预览估算最大字符数，和 UI 预览截断保持一致。
const SEARCH_RESULT_PREVIEW_ESTIMATE_CHARS: usize = 420;
/// 搜索结果列表最低内容宽度。
const SEARCH_RESULT_LIST_MIN_WIDTH: f32 = 760.0;
/// 搜索结果 ASCII 字符宽度估算；需要和 UI 结果行撑宽逻辑保持一致。
const SEARCH_RESULT_ASCII_CHAR_WIDTH_ESTIMATE: f32 = 7.4;
/// 搜索结果中文等非 ASCII 字符宽度估算；避免混排结果低估宽度导致横向滚动条缺失。
const SEARCH_RESULT_WIDE_CHAR_WIDTH_ESTIMATE: f32 = 13.0;
/// 目录搜索每发现一批目标就立即搜索，避免长时间停留在“准备目录搜索目标”阶段。
const DIRECTORY_SEARCH_TARGET_BATCH_SIZE: usize = 32;

/// 解析设置中的快搜关键字。
///
/// 参数说明：
/// - `raw_keywords`：用户在设置页输入的英文逗号分隔文本。
///
/// 返回值：去除空项和重复项后的关键字列表，保持首次出现顺序。
pub(crate) fn parse_quick_search_keywords(raw_keywords: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    raw_keywords
        .split(',')
        .filter_map(|keyword| {
            let keyword = keyword.trim();
            if keyword.is_empty() || !seen.insert(keyword.to_string()) {
                None
            } else {
                Some(keyword.to_string())
            }
        })
        .collect()
}

/// 计算分页日志跳转命中行时的垂直滚动偏移。
///
/// 参数说明：
/// - `line_number`：0 基命中行号。
/// - `line_count`：日志总行数。
/// - `viewport_height`：当前日志视口高度。
///
/// 返回值：将命中行尽量放到视口中间的滚动偏移，并限制在可滚动范围内。
fn centered_paged_scroll_top(line_number: usize, line_count: usize, viewport_height: f64) -> f64 {
    let row_height = LOG_VIEWER_ROW_HEIGHT as f64;
    let total_height = line_count as f64 * row_height;
    let max_top = (total_height - viewport_height).max(0.0);
    let target = line_number as f64 * row_height - viewport_height / 2.0 + row_height / 2.0;

    target.clamp(0.0, max_top)
}

/// 后台搜索线程向 UI 线程回传的事件。
enum SearchWorkerEvent {
    /// 目录搜索目标发现进度；可能携带补齐后的来源树快照。
    Prepared(Box<SearchPreparedEvent>),
    /// 搜索进度更新。
    Progress(SearchProgress),
    /// 搜索结果批次。
    Results(Vec<SearchResult>),
    /// 搜索启动或准备阶段失败。
    Failed(String),
    /// 搜索任务结束。
    Finished(SearchTaskSummary),
}

/// 搜索准备阶段回传给 UI 的状态；目录搜索会携带补齐后的来源树。
struct SearchPreparedEvent {
    /// 补齐懒加载节点后的来源树；只在目录搜索发现新节点时更新。
    registry: Option<SourceRegistry>,
    /// 当前已经发现的搜索目标文件数量。
    total_files: usize,
}

/// 后台目录搜索准备结果；仅用于回归测试旧的完整补齐逻辑。
#[cfg(test)]
struct DirectorySearchPreparation {
    /// 补齐懒加载节点后的来源树快照。
    registry: SourceRegistry,
    /// 目录下可搜索的日志目标。
    targets: Vec<SearchTarget>,
    /// 补齐子级过程中遇到的非致命错误。
    errors: Vec<String>,
}

/// 当前日志快速查找缓存命中后的导航方向。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QuickMatchAction {
    /// 跳转到下一个命中。
    Next,
    /// 跳转到上一个命中。
    Previous,
}

impl ArgusApp {
    /// 打开或置前独立日志搜索窗口，并聚焦关键字输入框。
    ///
    /// 参数说明：
    /// - `cx`：GPUI 上下文，用于创建独立无标题栏窗口。
    pub(crate) fn open_log_search_window(&mut self, cx: &mut Context<Self>) {
        if !self.ensure_active_log_tab_for_search() {
            self.placeholder_notice = "请先打开日志再搜索".to_string();
            return;
        }

        if self.log_search.is_window_open {
            if let Some(window_handle) = self.log_search.window_handle
                && window_handle
                    .update(cx, |_, window, _| window.activate_window())
                    .is_ok()
            {
                self.prepare_log_search_defaults();
                self.focus_log_search_keyword_for_open();
                self.placeholder_notice = "日志搜索窗口已显示到最前".to_string();
                return;
            }

            self.log_search.is_window_open = false;
            self.log_search.window_handle = None;
        }

        self.prepare_log_search_defaults();
        self.log_search.is_window_open = true;
        self.focus_log_search_keyword_for_open();

        let app_entity = cx.entity();
        let initial_theme = self.theme.clone();
        let initial_search = self.log_search.clone();
        let bounds = gpui::Bounds::centered(
            None,
            size(px(LOG_SEARCH_WINDOW_WIDTH), px(LOG_SEARCH_WINDOW_HEIGHT)),
            cx,
        );
        let window_options = WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(
                px(LOG_SEARCH_WINDOW_MIN_WIDTH),
                px(LOG_SEARCH_WINDOW_MIN_HEIGHT),
            )),
            ..Default::default()
        };

        match cx.open_window(window_options, move |_, cx| {
            cx.new(|cx| LogSearchWindow::new(app_entity, initial_theme, initial_search, cx))
        }) {
            Ok(window_handle) => {
                self.log_search.window_handle = Some(window_handle);
                self.placeholder_notice = "已打开日志搜索窗口".to_string();
            }
            Err(error) => {
                self.log_search.is_window_open = false;
                self.log_search.window_handle = None;
                self.log_search.message = Some(error.to_string());
                self.placeholder_notice = format!("打开日志搜索窗口失败：{error}");
            }
        }
    }

    /// 关闭独立日志搜索窗口；保留底部结果面板，便于继续查看搜索结果。
    pub(crate) fn close_log_search_window(&mut self) {
        self.log_search.is_window_open = false;
        self.log_search.window_handle = None;
        self.clear_log_search_input_focus();
        self.placeholder_notice = "已关闭日志搜索窗口".to_string();
    }

    /// 设置搜索范围。
    pub(crate) fn set_log_search_scope(&mut self, scope: SearchScope) {
        self.log_search.scope = scope;
        self.prepare_log_search_directory_default();
        self.placeholder_notice = format!("搜索范围已切换为{}", scope.label());
    }

    /// 切换搜索大小写敏感选项；只影响下一次启动的搜索任务。
    pub(crate) fn toggle_log_search_case_sensitive(&mut self) {
        self.log_search.case_sensitive = !self.log_search.case_sensitive;
        self.clear_quick_log_search_state();
        self.placeholder_notice = if self.log_search.case_sensitive {
            "搜索已启用区分大小写".to_string()
        } else {
            "搜索已关闭区分大小写".to_string()
        };
    }

    /// 切换正则搜索选项；关键字保持原样，由启动搜索时统一校验。
    pub(crate) fn toggle_log_search_regex_enabled(&mut self) {
        self.log_search.regex_enabled = !self.log_search.regex_enabled;
        self.clear_quick_log_search_state();
        self.placeholder_notice = if self.log_search.regex_enabled {
            "搜索已启用正则模式".to_string()
        } else {
            "搜索已关闭正则模式".to_string()
        };
    }

    /// 启动日志搜索；新任务会取消旧任务并使用新的 generation 隔离过期事件。
    ///
    /// 参数说明：
    /// - `scope`：搜索范围。
    /// - `cx`：GPUI 上下文，用于安排后台线程事件轮询。
    pub(crate) fn start_log_search(&mut self, scope: SearchScope, cx: &mut Context<Self>) {
        let keyword = self.log_search.keyword_input.value.trim().to_string();
        if keyword.is_empty() {
            self.log_search.message = Some("请输入搜索关键字".to_string());
            self.placeholder_notice = "请输入搜索关键字".to_string();
            return;
        }

        let query = SearchQuery {
            keyword,
            case_sensitive: self.log_search.case_sensitive,
            regex_enabled: self.log_search.regex_enabled,
        };
        if let Err(message) = SearchEngine::validate_query(&query) {
            self.log_search.message = Some(message.clone());
            self.placeholder_notice = message;
            return;
        }

        // 关键字校验通过，记录到搜索历史用于下次输入框下拉展示，并关闭历史下拉。
        self.record_search_keyword(&query.keyword);
        self.log_search.keyword_history_open = false;

        self.start_log_search_with_queries(scope, vec![query], SearchRunKind::Normal, cx);
    }

    /// 按当前搜索窗口范围启动一键快搜。
    ///
    /// 参数说明：
    /// - `cx`：GPUI 上下文，用于安排后台线程事件轮询。
    pub(crate) fn start_quick_keyword_search(&mut self, cx: &mut Context<Self>) {
        let keywords = parse_quick_search_keywords(&self.config.log_search.quick_keywords);
        if keywords.is_empty() {
            let message = "请先在设置中配置快搜关键字".to_string();
            self.log_search.message = Some(message.clone());
            self.placeholder_notice = message;
            return;
        }

        let queries = keywords
            .into_iter()
            .map(|keyword| SearchQuery {
                keyword,
                case_sensitive: self.log_search.case_sensitive,
                regex_enabled: self.log_search.regex_enabled,
            })
            .collect::<Vec<_>>();

        for query in &queries {
            if let Err(message) = SearchEngine::validate_query(query) {
                let message = if queries.len() > 1 {
                    format!("快搜关键字 `{}` {message}", query.keyword)
                } else {
                    message
                };
                self.log_search.message = Some(message.clone());
                self.placeholder_notice = message;
                return;
            }
        }

        self.start_log_search_with_queries(
            self.log_search.scope,
            queries,
            SearchRunKind::QuickKeywords,
            cx,
        );
    }

    /// 使用指定查询集合启动搜索任务；普通搜索和快搜共享此流程。
    fn start_log_search_with_queries(
        &mut self,
        scope: SearchScope,
        queries: Vec<SearchQuery>,
        run_kind: SearchRunKind,
        cx: &mut Context<Self>,
    ) {
        let directory_prepare = if scope == SearchScope::Directory {
            let directory_id = match self.resolve_search_directory_source_id() {
                Ok(directory_id) => directory_id,
                Err(message) => {
                    self.log_search.message = Some(message.clone());
                    self.placeholder_notice = message;
                    return;
                }
            };
            Some((
                directory_id,
                self.source_registry.clone(),
                self.config.loader.clone(),
            ))
        } else {
            None
        };

        let targets = if directory_prepare.is_none() {
            match self.search_targets_for_scope(scope) {
                Ok(targets) if !targets.is_empty() => targets,
                Ok(_) => {
                    self.log_search.message = Some("当前范围没有可搜索的日志文件".to_string());
                    self.placeholder_notice = "当前范围没有可搜索的日志文件".to_string();
                    return;
                }
                Err(message) => {
                    self.log_search.message = Some(message.clone());
                    self.placeholder_notice = message;
                    return;
                }
            }
        } else {
            Vec::new()
        };

        self.cancel_log_search();
        self.log_search.scope = scope;
        self.log_search.run_kind = run_kind;
        self.log_search.generation = self.log_search.generation.wrapping_add(1);
        self.log_search.progress = SearchProgress {
            total_files: targets.len(),
            ..SearchProgress::default()
        };
        self.log_search.task_state = SearchTaskState::Running;
        self.log_search.results.clear();
        self.log_search.result_keywords.clear();
        self.log_search.result_keyword_summary = None;
        self.log_search.result_groups.clear();
        self.log_search.visible_result_items.clear();
        self.log_search.collapsed_result_groups.clear();
        self.log_search.result_list_content_width = 0.0;
        self.log_search.active_result_index = None;
        self.log_search.pending_activation = None;
        self.log_search.result_scroll = gpui::UniformListScrollHandle::new();
        self.log_search.result_scrollbar_drag = None;
        self.log_search.result_panel_resize_drag = None;
        self.log_search.message = None;

        let generation = self.log_search.generation;
        let cancel_token = Arc::new(AtomicBool::new(false));
        self.log_search.cancel_token = Some(cancel_token.clone());
        let default_encoding = self.selected_encoding.clone();
        let archive_passwords = self.archive_passwords.clone();
        let (sender, receiver) = mpsc::channel::<SearchWorkerEvent>();

        if let Some((directory_id, registry, loader_config)) = directory_prepare {
            self.log_search.progress.current_path = Some("正在发现目录搜索目标".to_string());
            let request = SearchRequest::with_queries(queries, Vec::new(), default_encoding)
                .with_archive_passwords(archive_passwords);
            spawn_directory_search_worker(
                directory_id,
                registry,
                loader_config,
                request,
                cancel_token,
                sender,
            );
        } else {
            let request = SearchRequest::with_queries(queries, targets, default_encoding)
                .with_archive_passwords(archive_passwords);
            spawn_search_worker(request, cancel_token, sender);
        }

        Self::poll_log_search_worker_events(generation, receiver, cx);

        self.placeholder_notice = (if scope == SearchScope::Directory {
            "正在发现目录搜索目标".to_string()
        } else {
            let action = if run_kind == SearchRunKind::QuickKeywords {
                "正在快搜"
            } else {
                "正在搜索"
            };
            format!(
                "{action} {} 个日志文件",
                self.log_search.progress.total_files
            )
        })
        .to_string();
    }

    /// 轮询后台搜索线程事件，并把批量结果合并应用到 UI 状态。
    ///
    /// 参数说明：
    /// - `generation`：当前搜索代次，用于丢弃旧任务事件。
    /// - `receiver`：后台线程事件通道。
    /// - `cx`：GPUI 上下文，用于注册异步轮询任务。
    fn poll_log_search_worker_events(
        generation: usize,
        receiver: mpsc::Receiver<SearchWorkerEvent>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |view, cx| {
            loop {
                let mut prepared_event: Option<SearchPreparedEvent> = None;
                let mut latest_progress = None;
                let mut pending_results = Vec::new();
                let mut failed_message = None;
                let mut finished_summary = None;
                let mut should_continue = true;
                let mut receiver_disconnected = false;

                for _ in 0..LOG_SEARCH_MAX_EVENTS_PER_TICK {
                    match receiver.try_recv() {
                        Ok(SearchWorkerEvent::Prepared(mut event)) => {
                            if let Some(existing) = &mut prepared_event {
                                if event.registry.is_some() {
                                    existing.registry = event.registry.take();
                                }
                                existing.total_files = event.total_files;
                            } else {
                                prepared_event = Some(*event);
                            }
                        }
                        Ok(SearchWorkerEvent::Progress(progress)) => {
                            latest_progress = Some(progress);
                        }
                        Ok(SearchWorkerEvent::Results(mut results)) => {
                            pending_results.append(&mut results);
                        }
                        Ok(SearchWorkerEvent::Failed(message)) => {
                            failed_message = Some(message);
                            should_continue = false;
                            break;
                        }
                        Ok(SearchWorkerEvent::Finished(summary)) => {
                            finished_summary = Some(summary);
                            should_continue = false;
                            break;
                        }
                        Err(mpsc::TryRecvError::Empty) => break,
                        Err(mpsc::TryRecvError::Disconnected) => {
                            should_continue = false;
                            receiver_disconnected = true;
                            break;
                        }
                    }
                }

                if prepared_event.is_some()
                    || latest_progress.is_some()
                    || !pending_results.is_empty()
                    || failed_message.is_some()
                    || finished_summary.is_some()
                    || receiver_disconnected
                {
                    view.update(cx, |app, cx| {
                        if let Some(event) = prepared_event {
                            app.apply_search_worker_event(
                                generation,
                                SearchWorkerEvent::Prepared(Box::new(event)),
                            );
                        }
                        if let Some(progress) = latest_progress {
                            app.apply_search_progress(generation, progress);
                        }
                        if !pending_results.is_empty() {
                            app.append_search_results(generation, pending_results);
                        }
                        if let Some(message) = failed_message {
                            app.apply_search_worker_event(
                                generation,
                                SearchWorkerEvent::Failed(message),
                            );
                        }
                        if let Some(summary) = finished_summary {
                            app.apply_search_worker_event(
                                generation,
                                SearchWorkerEvent::Finished(summary),
                            );
                        }
                        if receiver_disconnected {
                            app.mark_search_worker_disconnected(generation);
                        }
                        cx.notify();
                    })
                    .ok();
                }

                if !should_continue {
                    break;
                }
                Timer::after(Duration::from_millis(LOG_SEARCH_UI_POLL_INTERVAL_MS)).await;
            }
        })
        .detach();
    }

    /// 统计当前日志中关键字出现次数。
    pub(crate) fn count_current_log_matches(&mut self, cx: &mut Context<Self>) {
        self.start_current_log_count_scan(cx);
    }

    /// 跳转到当前日志中的下一个关键字命中。
    pub(crate) fn activate_next_current_log_match(&mut self, cx: &mut Context<Self>) {
        if self.try_activate_cached_quick_match(QuickMatchAction::Next) {
            return;
        }
        self.start_current_log_navigation_scan(CurrentLogMatchDirection::Next, cx);
    }

    /// 跳转到当前日志中的上一个关键字命中。
    pub(crate) fn activate_previous_current_log_match(&mut self, cx: &mut Context<Self>) {
        if self.try_activate_cached_quick_match(QuickMatchAction::Previous) {
            return;
        }
        self.start_current_log_navigation_scan(CurrentLogMatchDirection::Previous, cx);
    }

    /// 清理当前日志快速查找状态，并取消尚未完成的扫描任务。
    pub(crate) fn clear_quick_log_search_state(&mut self) {
        if let Some(cancel_token) = self.log_search.quick_cancel_token.take() {
            cancel_token.store(true, Ordering::Relaxed);
        }
        self.log_search.quick_match_generation =
            self.log_search.quick_match_generation.wrapping_add(1);
        self.log_search.quick_match_key = None;
        self.log_search.quick_matches.clear();
        self.log_search.quick_match_count = 0;
        self.log_search.active_quick_match_index = None;
        self.log_search.quick_match_message = None;
        self.log_search.is_quick_counting = false;
        for state in self.log_tab_view_states.values_mut() {
            state.active_search_match = None;
        }
    }

    /// 取消当前搜索任务。
    pub(crate) fn cancel_log_search(&mut self) {
        let was_running = self.cancel_active_log_search_task();
        if was_running {
            self.log_search.task_state = SearchTaskState::Cancelled;
            self.log_search.message = Some("搜索已取消".to_string());
            self.placeholder_notice = "搜索已取消".to_string();
        }
    }

    /// 启动当前日志计数任务；只统计出现次数，不缓存每一个命中结果。
    fn start_current_log_count_scan(&mut self, cx: &mut Context<Self>) {
        let (_, handle, query, key) = match self.current_log_quick_scan_context() {
            Ok(context) => context,
            Err(message) => {
                self.log_search.quick_match_message = Some(message.clone());
                self.placeholder_notice = message;
                return;
            }
        };

        if let Some(cancel_token) = self.log_search.quick_cancel_token.take() {
            cancel_token.store(true, Ordering::Relaxed);
        }
        let generation = self.log_search.quick_match_generation.wrapping_add(1);
        self.log_search.quick_match_generation = generation;
        self.log_search.quick_match_key = Some(key.clone());
        self.log_search.quick_matches.clear();
        self.log_search.quick_match_count = 0;
        self.log_search.active_quick_match_index = None;
        self.log_search.quick_match_message = Some("计数中...".to_string());
        self.log_search.is_quick_counting = true;

        let cancel_token = Arc::new(AtomicBool::new(false));
        self.log_search.quick_cancel_token = Some(cancel_token.clone());
        cx.spawn(async move |view, cx| {
            let count_result = cx
                .background_executor()
                .spawn(async move {
                    SearchEngine::count_current_log_matches(handle, query, cancel_token)
                })
                .await;
            view.update(cx, |app, cx| {
                app.apply_current_log_count_scan(generation, key, count_result);
                cx.notify();
            })
            .ok();
        })
        .detach();

        self.placeholder_notice = "正在统计当前日志关键字".to_string();
    }

    /// 启动当前日志上/下一个快速定位扫描；只查找最近一个命中，避免大日志整文件计数。
    fn start_current_log_navigation_scan(
        &mut self,
        direction: CurrentLogMatchDirection,
        cx: &mut Context<Self>,
    ) {
        let (target, handle, query, key) = match self.current_log_quick_scan_context() {
            Ok(context) => context,
            Err(message) => {
                self.log_search.quick_match_message = Some(message.clone());
                self.placeholder_notice = message;
                return;
            }
        };
        let start_position = self.current_log_navigation_position(target.source_id, &handle);

        if let Some(cancel_token) = self.log_search.quick_cancel_token.take() {
            cancel_token.store(true, Ordering::Relaxed);
        }
        let generation = self.log_search.quick_match_generation.wrapping_add(1);
        self.log_search.quick_match_generation = generation;
        self.log_search.quick_match_key = Some(key.clone());
        self.log_search.quick_matches.clear();
        self.log_search.quick_match_count = 0;
        self.log_search.active_quick_match_index = None;
        self.log_search.quick_match_message = Some("定位中...".to_string());
        self.log_search.is_quick_counting = true;

        let cancel_token = Arc::new(AtomicBool::new(false));
        self.log_search.quick_cancel_token = Some(cancel_token.clone());
        cx.spawn(async move |view, cx| {
            let navigation_result = cx
                .background_executor()
                .spawn(async move {
                    SearchEngine::find_current_log_match(
                        target,
                        handle,
                        query,
                        start_position,
                        direction,
                        cancel_token,
                    )
                })
                .await;
            view.update(cx, |app, cx| {
                app.apply_current_log_navigation_scan(
                    generation,
                    key,
                    direction,
                    navigation_result,
                );
                cx.notify();
            })
            .ok();
        })
        .detach();

        self.placeholder_notice = "正在定位当前日志关键字".to_string();
    }

    /// 生成当前日志快速查找所需的目标、读取句柄和查询配置。
    fn current_log_quick_scan_context(
        &self,
    ) -> Result<
        (
            SearchTarget,
            crate::reader::log_file_reader::LogReaderHandle,
            SearchQuery,
            QuickMatchKey,
        ),
        String,
    > {
        let TabKind::LogSource { source_id, .. } = self.active_tab_kind() else {
            return Err("当前没有打开的日志文件".to_string());
        };
        let keyword = self.log_search.keyword_input.value.trim().to_string();
        if keyword.is_empty() {
            return Err("请输入搜索关键字".to_string());
        }
        let query = SearchQuery {
            keyword: keyword.clone(),
            case_sensitive: self.log_search.case_sensitive,
            regex_enabled: self.log_search.regex_enabled,
        };
        SearchEngine::validate_query(&query)?;

        let target = self
            .search_target_from_source(source_id)
            .ok_or_else(|| "当前标签不是可搜索日志".to_string())?;
        let handle = self
            .active_log_handle()
            .cloned()
            .ok_or_else(|| "当前日志尚未读取完成，请稍后再试".to_string())?;
        let key = QuickMatchKey {
            source_id,
            keyword,
            case_sensitive: self.log_search.case_sensitive,
            regex_enabled: self.log_search.regex_enabled,
        };

        Ok((target, handle, query, key))
    }

    /// 根据当前正文高亮或滚动位置生成上/下一个定位起点。
    ///
    /// 参数说明：
    /// - `source_id`：当前日志来源节点。
    /// - `handle`：当前日志读取句柄，用于限制行号范围。
    ///
    /// 返回值：当前命中位置；如果还没有激活命中，则从当前可见顶行开始。
    fn current_log_navigation_position(
        &self,
        source_id: SourceId,
        handle: &crate::reader::log_file_reader::LogReaderHandle,
    ) -> CurrentLogMatchPosition {
        let Some(state) = self.log_tab_view_states.get(&self.active_tab_id) else {
            return CurrentLogMatchPosition::default();
        };

        if let Some(active_match) = state
            .active_search_match
            .as_ref()
            .filter(|active_match| active_match.source_id == source_id)
        {
            let match_index = active_match
                .active_range
                .as_ref()
                .and_then(|active_range| {
                    active_match
                        .match_ranges
                        .iter()
                        .position(|range| range == active_range)
                })
                .or_else(|| (!active_match.match_ranges.is_empty()).then_some(0));
            return CurrentLogMatchPosition {
                line_number: active_match
                    .line_number
                    .min(handle.line_count().saturating_sub(1)),
                match_index,
            };
        }

        CurrentLogMatchPosition {
            line_number: ((state.paged_scroll.top_px / LOG_VIEWER_ROW_HEIGHT as f64).floor()
                as usize)
                .min(handle.line_count().saturating_sub(1)),
            match_index: None,
        }
    }

    /// 应用当前日志计数结果；计数结果只用于展示，不作为上/下一个定位缓存。
    fn apply_current_log_count_scan(
        &mut self,
        generation: usize,
        key: QuickMatchKey,
        count_result: Result<CurrentLogMatchCount, String>,
    ) {
        if self.log_search.quick_match_generation != generation
            || self.log_search.quick_match_key.as_ref() != Some(&key)
        {
            return;
        }

        self.log_search.quick_cancel_token = None;
        self.log_search.is_quick_counting = false;
        self.log_search.quick_match_key = None;
        self.log_search.quick_matches.clear();
        self.log_search.active_quick_match_index = None;

        let count = match count_result {
            Ok(count) => count,
            Err(message) => {
                self.log_search.quick_match_count = 0;
                self.log_search.quick_match_message = Some(message.clone());
                self.placeholder_notice = message;
                return;
            }
        };

        self.log_search.quick_match_count = count.match_count;

        if self.log_search.quick_match_count == 0 {
            self.log_search.quick_match_message = Some("未找到匹配项".to_string());
            self.placeholder_notice = "当前日志未找到匹配项".to_string();
            return;
        }

        let message = format!("共 {} 次", self.log_search.quick_match_count);
        self.log_search.quick_match_message = Some(message.clone());
        self.placeholder_notice = message;
    }

    /// 应用当前日志上/下一个快速定位结果。
    fn apply_current_log_navigation_scan(
        &mut self,
        generation: usize,
        key: QuickMatchKey,
        direction: CurrentLogMatchDirection,
        navigation_result: Result<CurrentLogMatchNavigation, String>,
    ) {
        if self.log_search.quick_match_generation != generation
            || self.log_search.quick_match_key.as_ref() != Some(&key)
        {
            return;
        }

        self.log_search.quick_cancel_token = None;
        self.log_search.is_quick_counting = false;
        self.log_search.quick_match_key = None;
        self.log_search.quick_matches.clear();
        self.log_search.quick_match_count = 0;
        self.log_search.active_quick_match_index = None;

        let navigation = match navigation_result {
            Ok(navigation) => navigation,
            Err(message) => {
                self.log_search.quick_match_message = Some(message.clone());
                self.placeholder_notice = message;
                return;
            }
        };

        let Some(result) = navigation.result else {
            self.log_search.quick_match_message = Some("未找到匹配项".to_string());
            self.placeholder_notice = "当前日志未找到匹配项".to_string();
            return;
        };
        let Some(active_range) = navigation.active_range else {
            self.log_search.quick_match_message = Some("未找到匹配项".to_string());
            self.placeholder_notice = "当前日志未找到匹配项".to_string();
            return;
        };

        self.apply_quick_match_highlight(&result, active_range);
        self.scroll_to_search_result(&result);
        let direction_label = match direction {
            CurrentLogMatchDirection::Next => "下一个",
            CurrentLogMatchDirection::Previous => "上一个",
        };
        let message = format!(
            "已定位{}：{} 第 {} 行",
            direction_label,
            result.label,
            result.line_number + 1
        );
        self.log_search.quick_match_message = Some(message.clone());
        self.placeholder_notice = message;
    }

    /// 尝试复用当前日志快速查找缓存执行上/下一个定位。
    fn try_activate_cached_quick_match(&mut self, action: QuickMatchAction) -> bool {
        let Ok((_, _, _, key)) = self.current_log_quick_scan_context() else {
            return false;
        };
        if self.log_search.quick_match_key.as_ref() != Some(&key) {
            return false;
        }
        if self.log_search.is_quick_counting {
            let message = self
                .log_search
                .quick_match_message
                .clone()
                .unwrap_or_else(|| "处理中...".to_string());
            self.log_search.quick_match_message = Some(message.clone());
            self.placeholder_notice = message;
            return true;
        }
        if self.log_search.quick_matches.is_empty() {
            return false;
        }
        if self.log_search.quick_match_count == 0 {
            self.log_search.quick_match_message = Some("未找到匹配项".to_string());
            self.placeholder_notice = "当前日志未找到匹配项".to_string();
            return true;
        }

        let count = self.log_search.quick_match_count;
        let current = self.log_search.active_quick_match_index;
        let next_index = match action {
            QuickMatchAction::Next => current.map(|index| (index + 1) % count).unwrap_or(0),
            QuickMatchAction::Previous => current
                .map(|index| if index == 0 { count - 1 } else { index - 1 })
                .unwrap_or(count - 1),
        };
        self.activate_quick_match_at_index(next_index);
        true
    }

    /// 根据出现序号定位到具体日志行和命中范围。
    fn activate_quick_match_at_index(&mut self, occurrence_index: usize) {
        let Some((result, active_range)) =
            quick_match_result_for_occurrence(&self.log_search.quick_matches, occurrence_index)
        else {
            self.log_search.quick_match_message = Some("未找到匹配项".to_string());
            self.placeholder_notice = "当前日志未找到匹配项".to_string();
            return;
        };

        self.log_search.active_quick_match_index = Some(occurrence_index);
        self.apply_quick_match_highlight(&result, active_range);
        self.scroll_to_search_result(&result);
        let message = format!(
            "第 {}/{} 次，{} 第 {} 行",
            occurrence_index + 1,
            self.log_search.quick_match_count,
            result.label,
            result.line_number + 1
        );
        self.log_search.quick_match_message = Some(message.clone());
        self.placeholder_notice = message;
    }

    /// 点击搜索结果后打开对应日志、滚动到行并高亮命中。
    pub(crate) fn activate_search_result(&mut self, result_index: usize, cx: &mut Context<Self>) {
        let Some(result) = self.log_search.results.get(result_index).cloned() else {
            self.placeholder_notice = "未找到搜索结果".to_string();
            return;
        };

        self.log_search.active_result_index = Some(result_index);
        self.open_or_focus_log_tab(result.source_id);
        self.request_open_log_content(result.source_id, cx);
        self.apply_search_result_highlight(&result);
        if !self.scroll_to_search_result(&result) {
            self.log_search.pending_activation = Some(result.clone());
        }
        self.placeholder_notice =
            format!("已定位到 {} 第 {} 行", result.label, result.line_number + 1);
    }

    /// 返回结果面板是否需要显示。
    pub(crate) fn should_show_log_search_results(&self) -> bool {
        !self.log_search.results.is_empty()
            || self.log_search.task_state.is_running()
            || self.log_search.message.is_some()
    }

    /// 关闭底部搜索结果面板并清理当前正文高亮。
    pub(crate) fn close_log_search_results_panel(&mut self) {
        self.cancel_active_log_search_task();
        self.log_search.task_state = SearchTaskState::Idle;
        self.clear_quick_log_search_state();
        self.log_search.results.clear();
        self.log_search.result_keywords.clear();
        self.log_search.result_keyword_summary = None;
        self.log_search.result_groups.clear();
        self.log_search.visible_result_items.clear();
        self.log_search.collapsed_result_groups.clear();
        self.log_search.result_list_content_width = 0.0;
        self.log_search.progress = SearchProgress::default();
        self.log_search.run_kind = SearchRunKind::Normal;
        self.log_search.active_result_index = None;
        self.log_search.pending_activation = None;
        self.log_search.result_scrollbar_drag = None;
        self.log_search.result_panel_resize_drag = None;
        self.log_search.message = None;
        self.placeholder_notice = "已关闭搜索结果面板".to_string();
    }

    /// 清理搜索运行期状态，通常在重新加载来源或关闭全部标签时调用。
    pub(crate) fn reset_log_search_runtime_state(&mut self) {
        self.clear_quick_log_search_state();
        self.cancel_active_log_search_task();
        self.log_search.progress = SearchProgress::default();
        self.log_search.task_state = SearchTaskState::Idle;
        self.log_search.run_kind = SearchRunKind::Normal;
        self.log_search.results.clear();
        self.log_search.result_keywords.clear();
        self.log_search.result_keyword_summary = None;
        self.log_search.result_groups.clear();
        self.log_search.visible_result_items.clear();
        self.log_search.collapsed_result_groups.clear();
        self.log_search.result_list_content_width = 0.0;
        self.log_search.active_result_index = None;
        self.log_search.pending_activation = None;
        self.log_search.result_scroll = gpui::UniformListScrollHandle::new();
        self.log_search.result_scrollbar_drag = None;
        self.log_search.result_panel_resize_drag = None;
        self.log_search.message = None;
        self.clear_log_search_history_inputs();
        self.selected_search_source_ids.clear();
        self.last_source_selection_anchor = None;
    }

    /// 来源树行点击入口，兼容普通打开、Ctrl/Cmd 多选和 Shift 范围选择。
    pub(crate) fn handle_source_tree_click(
        &mut self,
        source_id: SourceId,
        modifiers: Modifiers,
        cx: &mut Context<Self>,
    ) {
        self.clear_log_text_focus();

        let Some(source) = self.source_registry.node(source_id).cloned() else {
            self.placeholder_notice = "未找到来源节点".to_string();
            return;
        };

        if modifiers.shift {
            if !self.is_source_selectable_for_search_selection(source_id) {
                self.placeholder_notice = format!("{} 不是可选择的日志候选", source.label);
                return;
            }
            self.select_source_tree_range_for_search(source_id);
            self.placeholder_notice = format!(
                "已选择 {} 个搜索文件",
                self.selected_search_source_ids.len()
            );
            return;
        }

        if modifiers.secondary() {
            if !self.is_source_selectable_for_search_selection(source_id) {
                self.placeholder_notice = format!("{} 不是可选择的日志候选", source.label);
                return;
            }
            if !self.selected_search_source_ids.insert(source_id) {
                self.selected_search_source_ids.remove(&source_id);
            }
            self.last_source_selection_anchor = Some(source_id);
            self.placeholder_notice = format!(
                "已选择 {} 个搜索文件",
                self.selected_search_source_ids.len()
            );
            return;
        }

        if self.select_pending_archive_probe_for_search_anchor(source_id) {
            self.start_direct_source_archive_probe(source_id, source.clone(), cx);
            self.scroll_source_into_view(source_id);
            self.placeholder_notice = format!("已选择 {}，正在识别单文件日志", source.label);
            return;
        }

        if !source.kind.is_log_candidate() {
            return;
        }

        self.selected_search_source_ids.clear();
        self.selected_search_source_ids.insert(source_id);
        self.last_source_selection_anchor = Some(source_id);
        self.select_source(source_id);
        self.request_open_log_content(source_id, cx);
        self.scroll_source_into_view(source_id);
    }

    /// 返回来源节点是否属于搜索多选集合，用于来源树绘制选中态。
    pub(crate) fn is_source_selected_for_search(&self, source_id: SourceId) -> bool {
        self.selected_search_source_ids.contains(&source_id)
    }

    /// 将未完成单文件探测的压缩包设置为 Shift 范围选择锚点。
    ///
    /// 参数说明：
    /// - `source_id`：来源树中用户普通点击的压缩包节点。
    ///
    /// 返回值：节点确认为待探测压缩包时返回 `true`，调用方可继续触发后台识别。
    pub(crate) fn select_pending_archive_probe_for_search_anchor(
        &mut self,
        source_id: SourceId,
    ) -> bool {
        if !self.is_pending_archive_probe_candidate(source_id) {
            return false;
        }

        self.selected_search_source_ids.clear();
        self.selected_search_source_ids.insert(source_id);
        self.last_source_selection_anchor = Some(source_id);
        self.select_source(source_id);
        true
    }

    /// 返回输入框当前选区范围。
    pub(crate) fn log_search_input_selection_range(
        &self,
        input_kind: LogSearchInputKind,
    ) -> Option<Range<usize>> {
        let input = self.log_search_input(input_kind);
        let anchor = input.selection_anchor?;
        if anchor == input.cursor {
            return None;
        }

        Some(anchor.min(input.cursor)..anchor.max(input.cursor))
    }

    /// 聚焦指定搜索输入框。
    pub(crate) fn focus_log_search_input(&mut self, input_kind: LogSearchInputKind) {
        self.clear_log_search_input_focus();
        let input = self.log_search_input_mut(input_kind);
        input.is_focused = true;
        input.cursor = character_count(&input.value);
        input.selection_anchor = None;
        input.marked_range = None;
        input.selection_drag = None;
        // 聚焦关键字输入框时展开历史下拉（历史非空才展开），便于复用近期搜索。
        if input_kind == LogSearchInputKind::Keyword
            && !self.config.log_search.recent_keywords.is_empty()
        {
            self.open_keyword_history();
        }
    }

    /// 清空指定搜索输入框，并保持输入焦点。
    pub(crate) fn clear_log_search_input(&mut self, input_kind: LogSearchInputKind) {
        let input = self.log_search_input_mut(input_kind);
        input.value.clear();
        input.cursor = 0;
        input.selection_anchor = None;
        input.marked_range = None;
        input.selection_drag = None;
        input.is_focused = true;
        if input_kind == LogSearchInputKind::Keyword {
            self.clear_quick_log_search_state();
        }
    }

    /// 记录一次成功搜索的关键字到历史：去重后置于最前、截断到上限并持久化。
    pub(crate) fn record_search_keyword(&mut self, keyword: &str) {
        let keyword = keyword.trim();
        if keyword.is_empty() {
            return;
        }
        let recent = &mut self.config.log_search.recent_keywords;
        recent.retain(|item| item != keyword);
        recent.insert(0, keyword.to_string());
        recent.truncate(SEARCH_RECENT_KEYWORDS_MAX);
        self.persist_config_or_report();
    }

    /// 返回用于下拉展示的关键字历史条目，保持历史原始顺序（最新在前）。
    ///
    /// 历史下拉始终展示全部最近关键字：输入框可能残留上次搜索的关键字，若按其做子串
    /// 过滤，下拉会只剩"当前关键字"一项，无法起到挑选其它历史关键字的作用。
    pub(crate) fn keyword_history_items(&self) -> Vec<String> {
        self.config.log_search.recent_keywords.clone()
    }

    /// 打开关键字历史下拉菜单并重置高亮。
    pub(crate) fn open_keyword_history(&mut self) {
        self.log_search.keyword_history_open = true;
        self.log_search.keyword_history_highlight = None;
    }

    /// 关闭关键字历史下拉菜单并清空高亮。
    pub(crate) fn close_keyword_history(&mut self) {
        self.log_search.keyword_history_open = false;
        self.log_search.keyword_history_highlight = None;
    }

    /// 重置关键字历史下拉高亮索引，但保持下拉展开状态。
    ///
    /// 输入文本变化或选区改变后历史条目集合随之变化，原高亮索引会错位；此时清空高亮，
    /// 下拉仍展开以便实时过滤，下一次方向键会重新落到首/末项。
    pub(crate) fn reset_keyword_history_highlight(&mut self) {
        self.log_search.keyword_history_highlight = None;
    }

    /// 在关键字历史下拉中按 `delta` 移动高亮项；越过边界时循环到对端。
    pub(crate) fn move_keyword_history_highlight(&mut self, delta: isize) {
        if !self.log_search.keyword_history_open {
            return;
        }
        let count = self.config.log_search.recent_keywords.len();
        if count == 0 {
            self.log_search.keyword_history_highlight = None;
            return;
        }
        // 未选中时按向下落到首项、按向上落到末项；已选中则循环移动。
        let current = self
            .log_search
            .keyword_history_highlight
            .map(|index| index as isize)
            .unwrap_or(-1);
        let next = if current == -1 {
            if delta > 0 { 0 } else { count as isize - 1 }
        } else {
            (current + delta).rem_euclid(count as isize)
        };
        self.log_search.keyword_history_highlight = Some(next as usize);
    }

    /// 选中历史下拉中指定索引的关键字：填入输入框并立即触发搜索。
    pub(crate) fn select_keyword_history(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(keyword) = self.config.log_search.recent_keywords.get(index).cloned() {
            self.set_log_search_input_value(LogSearchInputKind::Keyword, &keyword);
            self.start_log_search(self.log_search.scope, cx);
        }
    }

    /// 将指定搜索输入框的值整体替换为 `text`，光标置于末尾并保持焦点。
    fn set_log_search_input_value(&mut self, input_kind: LogSearchInputKind, text: &str) {
        let input = self.log_search_input_mut(input_kind);
        input.value = text.to_string();
        input.cursor = character_count(&input.value);
        input.selection_anchor = None;
        input.marked_range = None;
        input.selection_drag = None;
        input.is_focused = true;
        if input_kind == LogSearchInputKind::Keyword {
            self.clear_quick_log_search_state();
        }
    }

    /// 处理搜索窗口输入框键盘输入。
    pub(crate) fn handle_log_search_input_key(
        &mut self,
        input_kind: LogSearchInputKind,
        keystroke: &Keystroke,
        cx: &mut Context<Self>,
    ) {
        let key = keystroke.key.to_lowercase();
        if keystroke.modifiers.secondary() {
            match key.as_str() {
                "a" => self.select_all_log_search_input(input_kind),
                "c" => self.copy_log_search_input_selection(input_kind, cx),
                "x" => self.cut_log_search_input_selection(input_kind, cx),
                "v" => self.paste_log_search_input_clipboard(input_kind, cx),
                "left" | "arrowleft" => {
                    self.move_log_search_input_cursor(input_kind, 0, keystroke.modifiers.shift)
                }
                "right" | "arrowright" => {
                    let end = character_count(&self.log_search_input(input_kind).value);
                    self.move_log_search_input_cursor(input_kind, end, keystroke.modifiers.shift);
                }
                _ => {}
            }
            return;
        }

        // 关键字历史下拉展开时，优先消费导航键，避免方向键移动光标或 Esc 误关窗口。
        if input_kind == LogSearchInputKind::Keyword && self.log_search.keyword_history_open {
            match key.as_str() {
                "up" | "arrowup" => {
                    self.move_keyword_history_highlight(-1);
                    return;
                }
                "down" | "arrowdown" => {
                    self.move_keyword_history_highlight(1);
                    return;
                }
                "enter" => {
                    if let Some(index) = self.log_search.keyword_history_highlight {
                        self.select_keyword_history(index, cx);
                        return;
                    }
                    self.start_log_search(self.log_search.scope, cx);
                    return;
                }
                "escape" => {
                    self.close_keyword_history();
                    return;
                }
                _ => {}
            }
        }

        match key.as_str() {
            "backspace" => self.delete_log_search_input_backward(input_kind),
            "delete" => self.delete_log_search_input_forward(input_kind),
            "escape" => self.close_log_search_window(),
            "enter" => self.start_log_search(self.log_search.scope, cx),
            "left" | "arrowleft" => {
                self.move_log_search_input_left(input_kind, keystroke.modifiers.shift)
            }
            "right" | "arrowright" => {
                self.move_log_search_input_right(input_kind, keystroke.modifiers.shift)
            }
            "home" => self.move_log_search_input_cursor(input_kind, 0, keystroke.modifiers.shift),
            "end" => {
                let end = character_count(&self.log_search_input(input_kind).value);
                self.move_log_search_input_cursor(input_kind, end, keystroke.modifiers.shift);
            }
            _ => {
                if let Some(key_char) = keystroke.key_char.as_ref()
                    && !keystroke.modifiers.control
                    && !keystroke.modifiers.alt
                    && !keystroke.modifiers.platform
                    && !keystroke.modifiers.function
                    && !key_char.chars().any(char::is_control)
                {
                    self.insert_log_search_input_text(input_kind, key_char);
                }
            }
        }
    }

    /// 根据鼠标按下位置开始搜索输入框选择。
    pub(crate) fn begin_log_search_input_pointer_selection(
        &mut self,
        input_kind: LogSearchInputKind,
        character_index: usize,
        granularity: TextSelectionGranularity,
    ) {
        self.focus_log_search_input(input_kind);
        self.log_search_input_mut(input_kind)
            .begin_pointer_selection(character_index, granularity);
    }

    /// 鼠标拖拽过程中扩展搜索输入框选区。
    pub(crate) fn update_log_search_input_pointer_selection(
        &mut self,
        input_kind: LogSearchInputKind,
        character_index: usize,
    ) {
        self.log_search_input_mut(input_kind)
            .update_pointer_selection(character_index);
    }

    /// 结束搜索输入框鼠标选择。
    pub(crate) fn finish_log_search_input_pointer_selection(
        &mut self,
        input_kind: LogSearchInputKind,
    ) {
        let input = self.log_search_input_mut(input_kind);
        input.finish_pointer_selection();
    }

    /// 读取成功后执行等待中的搜索结果跳转。
    pub(crate) fn finish_pending_search_activation(&mut self, source_id: SourceId) {
        let Some(result) = self.log_search.pending_activation.clone() else {
            return;
        };
        if result.source_id != source_id {
            return;
        }
        self.apply_search_result_highlight(&result);
        if self.scroll_to_search_result(&result) {
            self.log_search.pending_activation = None;
        }
    }

    /// 应用后台线程回传的搜索事件。
    fn apply_search_worker_event(&mut self, generation: usize, event: SearchWorkerEvent) {
        if self.log_search.generation != generation {
            return;
        }

        match event {
            SearchWorkerEvent::Prepared(event) => {
                if let Some(registry) = event.registry {
                    self.source_registry = registry;
                    self.sync_source_tree_selection_from_active_tab();
                    self.rebuild_filtered_source_ids();
                }
                self.log_search.progress.total_files = event.total_files;
                self.log_search.message =
                    Some(format!("已发现 {} 个搜索目标，正在搜索", event.total_files));
                self.placeholder_notice = self
                    .log_search
                    .message
                    .clone()
                    .unwrap_or_else(|| "正在搜索已发现目标".to_string());
            }
            SearchWorkerEvent::Progress(progress) => {
                self.apply_search_progress(generation, progress)
            }
            SearchWorkerEvent::Results(results) => self.append_search_results(generation, results),
            SearchWorkerEvent::Failed(message) => {
                self.log_search.task_state = SearchTaskState::Failed(message.clone());
                self.log_search.cancel_token = None;
                self.log_search.message = Some(message.clone());
                self.placeholder_notice = message;
            }
            SearchWorkerEvent::Finished(summary) => {
                if summary.was_cancelled {
                    self.log_search.task_state = SearchTaskState::Cancelled;
                    self.log_search.message = Some("搜索已取消".to_string());
                } else if summary.errors.is_empty() {
                    self.log_search.task_state = SearchTaskState::Finished;
                    self.log_search.message =
                        Some(format!("搜索完成，找到 {} 条结果", summary.matched_results));
                } else {
                    self.log_search.task_state = SearchTaskState::Finished;
                    self.log_search.message = Some(format!(
                        "搜索完成，找到 {} 条结果，{} 个文件失败",
                        summary.matched_results,
                        summary.errors.len()
                    ));
                }
                self.log_search.cancel_token = None;
                self.placeholder_notice = self
                    .log_search
                    .message
                    .clone()
                    .unwrap_or_else(|| "搜索完成".to_string());
            }
        }
    }

    /// 应用搜索进度；generation 不一致时丢弃。
    pub(crate) fn apply_search_progress(&mut self, generation: usize, progress: SearchProgress) {
        if self.log_search.generation != generation {
            return;
        }
        self.log_search.progress = progress;
    }

    /// 标记后台搜索通道异常断开，避免 UI 一直停留在“搜索中”。
    ///
    /// 参数说明：
    /// - `generation`：断开通道所属搜索代次；旧代次会被忽略。
    fn mark_search_worker_disconnected(&mut self, generation: usize) {
        if self.log_search.generation != generation || !self.log_search.task_state.is_running() {
            return;
        }

        let message = "搜索任务已中断，请重试".to_string();
        self.log_search.cancel_token = None;
        self.log_search.task_state = SearchTaskState::Failed(message.clone());
        self.log_search.message = Some(message.clone());
        self.placeholder_notice = message;
    }

    /// 取消当前活跃搜索任务并推进 generation，让旧后台事件立即失效。
    ///
    /// 返回值：存在运行中任务返回 `true`，用于调用方决定是否展示取消提示。
    fn cancel_active_log_search_task(&mut self) -> bool {
        let was_running = self.log_search.task_state.is_running();
        let had_cancel_token = if let Some(cancel_token) = self.log_search.cancel_token.take() {
            cancel_token.store(true, Ordering::Relaxed);
            true
        } else {
            false
        };

        if was_running || had_cancel_token {
            self.log_search.generation = self.log_search.generation.wrapping_add(1);
            true
        } else {
            false
        }
    }

    /// 追加搜索结果批次；generation 不一致时丢弃。
    pub(crate) fn append_search_results(
        &mut self,
        generation: usize,
        mut results: Vec<SearchResult>,
    ) {
        if self.log_search.generation != generation {
            return;
        }
        let base_index = self.log_search.results.len();
        for (offset, result) in results.iter().enumerate() {
            self.append_search_result_list_item(base_index + offset, result);
            self.log_search.result_list_content_width = self
                .log_search
                .result_list_content_width
                .max(estimated_search_result_row_width(result));
        }
        self.append_search_result_keywords(&results);
        self.log_search.results.append(&mut results);
    }

    /// 增量维护搜索结果标题栏关键字摘要，避免渲染期扫描全部结果。
    fn append_search_result_keywords(&mut self, results: &[SearchResult]) {
        for result in results {
            for keyword in &result.matched_keywords {
                let keyword = keyword.trim();
                if !keyword.is_empty() {
                    self.log_search.result_keywords.insert(keyword.to_string());
                }
            }
        }
        self.rebuild_search_result_keyword_summary();
    }

    /// 根据已缓存关键字集合生成搜索结果面板标题摘要。
    fn rebuild_search_result_keyword_summary(&mut self) {
        if self.log_search.result_keywords.is_empty() {
            let keyword = self.log_search.keyword_input.value.trim();
            self.log_search.result_keyword_summary =
                (!keyword.is_empty()).then(|| format!("关键字：{keyword}"));
            return;
        }

        let keyword_count = self.log_search.result_keywords.len();
        let visible_keywords = self
            .log_search
            .result_keywords
            .iter()
            .take(6)
            .cloned()
            .collect::<Vec<_>>();
        let overflow_text = keyword_count
            .checked_sub(visible_keywords.len())
            .filter(|count| *count > 0)
            .map(|count| format!(" 等 {count} 个"))
            .unwrap_or_default();
        self.log_search.result_keyword_summary = Some(format!(
            "关键字：{}{}",
            visible_keywords.join("、"),
            overflow_text
        ));
    }

    /// 切换搜索结果文件分组展开状态。
    pub(crate) fn toggle_search_result_group(&mut self, group_index: usize) {
        let Some(group) = self.log_search.result_groups.get(group_index) else {
            self.placeholder_notice = "未找到搜索结果分组".to_string();
            return;
        };
        let source_id = group.source_id;
        let label = group.label.clone();

        if !self.log_search.collapsed_result_groups.insert(source_id) {
            self.log_search.collapsed_result_groups.remove(&source_id);
        }
        self.rebuild_visible_search_result_items();
        self.placeholder_notice = format!("已切换 {label} 的搜索结果展开状态");
    }

    /// 展开全部搜索结果文件分组。
    pub(crate) fn expand_all_search_result_groups(&mut self) {
        if self.log_search.result_groups.is_empty() {
            self.placeholder_notice = "暂无可展开的搜索结果".to_string();
            return;
        }

        self.log_search.collapsed_result_groups.clear();
        self.rebuild_visible_search_result_items();
        self.placeholder_notice = "已展开全部搜索结果".to_string();
    }

    /// 收起全部搜索结果文件分组。
    pub(crate) fn collapse_all_search_result_groups(&mut self) {
        if self.log_search.result_groups.is_empty() {
            self.placeholder_notice = "暂无可收起的搜索结果".to_string();
            return;
        }

        self.log_search.collapsed_result_groups = self
            .log_search
            .result_groups
            .iter()
            .map(|group| group.source_id)
            .collect();
        self.rebuild_visible_search_result_items();
        self.placeholder_notice = "已收起全部搜索结果".to_string();
    }

    /// 开始拖拽搜索结果面板高度。
    pub(crate) fn begin_search_result_panel_resize(&mut self, cursor_y: Pixels) {
        self.log_search.result_panel_resize_drag = Some(SearchResultPanelResizeDrag {
            start_y: cursor_y,
            start_height: self.log_search.result_panel_height,
        });
    }

    /// 拖拽更新搜索结果面板高度。
    ///
    /// `max_height` 为当前允许的面板最大高度，由调用方按窗口视口高度动态计算，
    /// 使面板可近乎撑满窗口；传入值小于最小高度时回退到最小高度，避免 clamp 区间反转。
    pub(crate) fn resize_search_result_panel(&mut self, cursor_y: Pixels, max_height: f32) -> bool {
        let Some(drag) = self.log_search.result_panel_resize_drag else {
            return false;
        };
        let delta = f32::from(drag.start_y - cursor_y);
        let upper = max_height.max(SEARCH_RESULT_PANEL_HEIGHT_MIN);
        let next_height = (drag.start_height + delta).clamp(SEARCH_RESULT_PANEL_HEIGHT_MIN, upper);
        if (next_height - self.log_search.result_panel_height).abs() < f32::EPSILON {
            return false;
        }

        self.log_search.result_panel_height = next_height;
        true
    }

    /// 结束搜索结果面板高度拖拽。
    pub(crate) fn finish_search_result_panel_resize(&mut self) -> bool {
        let was_resizing = self.log_search.result_panel_resize_drag.is_some();
        self.log_search.result_panel_resize_drag = None;
        was_resizing
    }

    /// 从当前 UI 状态推导搜索窗口默认值。
    fn prepare_log_search_defaults(&mut self) {
        self.prepare_log_search_keyword_default();
        self.prepare_log_search_directory_default();
    }

    /// 根据当前日志选区更新默认关键字；没有选区时保留上一次关键字。
    fn prepare_log_search_keyword_default(&mut self) {
        let Some(selected_text) = self
            .selected_log_text()
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
        else {
            return;
        };

        let keyword = &mut self.log_search.keyword_input;
        let should_clear_quick = keyword.value != selected_text;
        keyword.value = selected_text;
        keyword.cursor = character_count(&keyword.value);
        keyword.selection_anchor = None;
        keyword.marked_range = None;
        keyword.selection_drag = None;
        if should_clear_quick {
            self.clear_quick_log_search_state();
        }
    }

    /// 搜索窗口被唤起时聚焦关键字输入框；若已有内容则自动全选，便于直接覆盖搜索。
    fn focus_log_search_keyword_for_open(&mut self) {
        self.clear_log_search_input_focus();
        let keyword = &mut self.log_search.keyword_input;
        keyword.is_focused = true;
        keyword.cursor = character_count(&keyword.value);
        keyword.selection_anchor = (keyword.cursor > 0).then_some(0);
        keyword.marked_range = None;
        keyword.selection_drag = None;
    }

    /// 追加一条结果对应的分组行和结果行索引。
    fn append_search_result_list_item(&mut self, result_index: usize, result: &SearchResult) {
        let collapsed = self
            .log_search
            .collapsed_result_groups
            .contains(&result.source_id);

        if let Some(last_group) = self.log_search.result_groups.last_mut()
            && last_group.source_id == result.source_id
        {
            last_group.end_index = result_index + 1;
            if !collapsed {
                self.log_search
                    .visible_result_items
                    .push(SearchResultListItem::Result(result_index));
            }
            return;
        }

        let group_index = self.log_search.result_groups.len();
        self.log_search.result_groups.push(SearchResultGroup {
            source_id: result.source_id,
            label: result.label.clone(),
            path: result.path.clone(),
            start_index: result_index,
            end_index: result_index + 1,
        });
        self.log_search
            .visible_result_items
            .push(SearchResultListItem::Group(group_index));
        if !collapsed {
            self.log_search
                .visible_result_items
                .push(SearchResultListItem::Result(result_index));
        }
    }

    /// 根据折叠状态重建搜索结果虚拟列表可见行。
    fn rebuild_visible_search_result_items(&mut self) {
        self.log_search.visible_result_items.clear();
        for (group_index, group) in self.log_search.result_groups.iter().enumerate() {
            self.log_search
                .visible_result_items
                .push(SearchResultListItem::Group(group_index));
            if self
                .log_search
                .collapsed_result_groups
                .contains(&group.source_id)
            {
                continue;
            }
            self.log_search
                .visible_result_items
                .extend((group.start_index..group.end_index).map(SearchResultListItem::Result));
        }
    }

    /// 依据当前日志 tab 找到最近父目录并同步目录输入框。
    fn prepare_log_search_directory_default(&mut self) {
        let Some(directory_id) = self.default_search_directory_source_id() else {
            return;
        };
        let Some(directory) = self.source_registry.node(directory_id) else {
            return;
        };
        self.log_search.directory_source_id = Some(directory_id);
        self.log_search.directory_input.value = directory.location.display_path();
        self.log_search.directory_input.cursor =
            character_count(&self.log_search.directory_input.value);
        self.log_search.directory_input.selection_anchor = None;
        self.log_search.directory_input.marked_range = None;
        self.log_search.directory_input.selection_drag = None;
    }

    /// 返回当前日志所在的最近来源目录。
    fn default_search_directory_source_id(&self) -> Option<SourceId> {
        let TabKind::LogSource { source_id, .. } = self.active_tab_kind() else {
            return None;
        };

        self.source_registry
            .ancestor_ids(source_id)
            .into_iter()
            .rev()
            .find(|ancestor_id| {
                self.source_registry.node(*ancestor_id).is_some_and(|node| {
                    matches!(
                        node.kind,
                        SourceKind::Directory
                            | SourceKind::Archive(_)
                            | SourceKind::ArchiveDirectory
                    )
                })
            })
    }

    /// 根据搜索范围收集可搜索目标。
    fn search_targets_for_scope(
        &mut self,
        scope: SearchScope,
    ) -> Result<Vec<SearchTarget>, String> {
        match scope {
            SearchScope::CurrentFile => {
                let TabKind::LogSource { source_id, .. } = self.active_tab_kind() else {
                    return Err("当前没有打开的日志文件".to_string());
                };
                self.search_target_from_source(source_id)
                    .map(|target| vec![target])
                    .ok_or_else(|| "当前标签不是可搜索日志".to_string())
            }
            SearchScope::SelectedFiles => {
                let targets = self
                    .selected_search_source_ids
                    .iter()
                    .filter_map(|source_id| self.search_target_from_source(*source_id))
                    .collect::<Vec<_>>();
                if targets.is_empty() {
                    Err("请先在左侧来源树中选择一个或多个日志文件".to_string())
                } else {
                    Ok(targets)
                }
            }
            SearchScope::Directory => {
                let directory_id = self.resolve_search_directory_source_id()?;
                let targets = self.collect_loaded_log_targets_under(directory_id);
                if targets.is_empty() {
                    Err("目录下没有日志文件".to_string())
                } else {
                    Ok(targets)
                }
            }
        }
    }

    /// 从目录输入框精确解析来源树目录。
    fn resolve_search_directory_source_id(&self) -> Result<SourceId, String> {
        let directory_path = self.log_search.directory_input.value.trim();
        if directory_path.is_empty() {
            return Err("请输入来源树目录路径".to_string());
        }

        let matches = self
            .source_registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|source_id| {
                let node = self.source_registry.node(*source_id)?;
                if matches!(
                    node.kind,
                    SourceKind::Directory | SourceKind::Archive(_) | SourceKind::ArchiveDirectory
                ) && node.location.display_path() == directory_path
                {
                    Some(*source_id)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        match matches.as_slice() {
            [source_id] => Ok(*source_id),
            [] => Err("未在来源树中找到该目录".to_string()),
            _ => Err("来源树中存在多个同名目录路径，请选择更精确的目录".to_string()),
        }
    }

    /// 收集指定目录下已经加载到来源树中的所有日志候选。
    fn collect_loaded_log_targets_under(&self, directory_id: SourceId) -> Vec<SearchTarget> {
        let mut targets = Vec::new();
        self.collect_loaded_log_targets_recursive(directory_id, &mut targets);
        targets
    }

    /// 递归收集来源树中的日志候选；目录搜索的懒加载补齐由后台准备流程负责。
    fn collect_loaded_log_targets_recursive(
        &self,
        parent_id: SourceId,
        targets: &mut Vec<SearchTarget>,
    ) {
        for child_id in self.source_registry.child_ids(parent_id).iter().copied() {
            if let Some(target) = self.search_target_from_source(child_id) {
                targets.push(target);
                continue;
            }

            if self
                .source_registry
                .node(child_id)
                .is_some_and(|node| node.kind.can_expand())
            {
                self.collect_loaded_log_targets_recursive(child_id, targets);
            }
        }
    }

    /// 将来源树日志节点转换为搜索目标。
    fn search_target_from_source(&self, source_id: SourceId) -> Option<SearchTarget> {
        let node = self.source_registry.node(source_id)?;
        if !node.kind.is_log_candidate() {
            return None;
        }

        Some(SearchTarget {
            source_id,
            label: node.label.clone(),
            path: node.location.display_path(),
            location: node.location.clone(),
        })
    }

    /// 按可见来源树顺序执行 Shift 范围多选。
    pub(crate) fn select_source_tree_range_for_search(&mut self, target_id: SourceId) {
        let Some(anchor_id) = self.last_source_selection_anchor else {
            self.selected_search_source_ids.clear();
            self.selected_search_source_ids.insert(target_id);
            self.last_source_selection_anchor = Some(target_id);
            return;
        };
        let visible_ids = self.visible_source_ids().to_vec();
        let Some(mut selected) =
            self.source_tree_range_selection_from_order(&visible_ids, anchor_id, target_id, None)
        else {
            // 原锚点已经不在当前可见树中时，用本次目标重建锚点，避免后续范围基于失效节点。
            self.selected_search_source_ids.clear();
            self.selected_search_source_ids.insert(target_id);
            self.last_source_selection_anchor = Some(target_id);
            return;
        };

        if self.is_source_archive_probe_running_for_selection() {
            let mut stable_order_allowed_ids = visible_ids.iter().copied().collect::<BTreeSet<_>>();
            stable_order_allowed_ids.extend(
                self.source_registry
                    .tree_order_source_ids()
                    .iter()
                    .copied()
                    .filter(|source_id| self.is_pending_archive_probe_candidate(*source_id)),
            );
            if let Some(stable_order_selected) = self.source_tree_range_selection_from_order(
                self.source_registry.tree_order_source_ids(),
                anchor_id,
                target_id,
                Some(&stable_order_allowed_ids),
            ) && stable_order_selected.len() > selected.len()
            {
                selected = stable_order_selected;
            }
        }

        if !selected.is_empty() {
            self.selected_search_source_ids = selected;
        }
    }

    /// 按指定来源 ID 顺序计算 Shift 范围选区。
    ///
    /// 参数说明：
    /// - `ordered_ids`：可见顺序或稳定树序。
    /// - `anchor_id`：范围起点。
    /// - `target_id`：本次点击的范围终点。
    /// - `allowed_ids`：使用稳定树序兜底时允许纳入的节点集合；为空时不做额外限制。
    ///
    /// 返回值：锚点和目标均存在时返回可选择来源集合，否则返回 `None`。
    fn source_tree_range_selection_from_order(
        &self,
        ordered_ids: &[SourceId],
        anchor_id: SourceId,
        target_id: SourceId,
        allowed_ids: Option<&BTreeSet<SourceId>>,
    ) -> Option<BTreeSet<SourceId>> {
        let anchor_index = ordered_ids.iter().position(|id| *id == anchor_id)?;
        let target_index = ordered_ids.iter().position(|id| *id == target_id)?;
        let (start, end) = if anchor_index <= target_index {
            (anchor_index, target_index)
        } else {
            (target_index, anchor_index)
        };

        Some(
            ordered_ids[start..=end]
                .iter()
                .filter(|source_id| {
                    allowed_ids
                        .map(|allowed_ids| allowed_ids.contains(source_id))
                        .unwrap_or(true)
                })
                .filter(|source_id| self.is_source_selectable_for_search_selection(**source_id))
                .copied()
                .collect::<BTreeSet<_>>(),
        )
    }

    /// 判断来源节点是否可参与来源树多选。
    ///
    /// 说明：单文件压缩包探测未完成前仍是 `Archive`，但用户已经能在树中看到它；
    /// 允许其临时进入多选集合，探测完成后如果变成 `SingleFileArchive` 会自然成为日志候选。
    pub(crate) fn is_source_selectable_for_search_selection(&self, source_id: SourceId) -> bool {
        self.source_registry.node(source_id).is_some_and(|node| {
            node.kind.is_log_candidate() || self.is_pending_archive_probe_candidate(source_id)
        })
    }

    /// 判断来源节点是否是单文件压缩包探测完成前的临时可选节点。
    fn is_pending_archive_probe_candidate(&self, source_id: SourceId) -> bool {
        self.source_registry.node(source_id).is_some_and(|node| {
            matches!(node.kind, SourceKind::Archive(_))
                && !node.metadata.children_loaded
                && !self.source_archive_probe_completed_ids.contains(&source_id)
        })
    }

    /// 返回来源树单文件压缩包探测是否仍有排队或执行中的任务。
    fn is_source_archive_probe_running_for_selection(&self) -> bool {
        !self.source_archive_probe_queue.is_empty()
            || !self.source_archive_probe_queued_ids.is_empty()
            || !self.source_archive_probe_inflight_ids.is_empty()
            || !self.source_archive_probe_direct_inflight_ids.is_empty()
    }

    /// 写入搜索结果高亮状态。
    fn apply_search_result_highlight(&mut self, result: &SearchResult) {
        if let Some(tab_id) = self
            .tabs
            .iter()
            .find(|tab| {
                matches!(
                    tab.kind,
                    TabKind::LogSource {
                        source_id,
                        ..
                    } if source_id == result.source_id
                )
            })
            .map(|tab| tab.id)
        {
            let state = self.log_tab_view_states.entry(tab_id).or_default();
            state.active_search_match = Some(super::ActiveSearchMatch {
                source_id: result.source_id,
                line_number: result.line_number,
                match_ranges: result.match_ranges.clone(),
                // 搜索结果点击定位时需要同时激活一个具体命中片段，否则正文中只有行背景，
                // 关键字仍使用普通命中样式，在选中行背景上对比度不足。
                active_range: result.match_ranges.first().cloned(),
            });
        }
    }

    /// 写入当前日志快速查找高亮状态，额外标记当前激活的单个命中范围。
    fn apply_quick_match_highlight(&mut self, result: &SearchResult, active_range: Range<usize>) {
        if let Some(tab_id) = self
            .tabs
            .iter()
            .find(|tab| {
                matches!(
                    tab.kind,
                    TabKind::LogSource {
                        source_id,
                        ..
                    } if source_id == result.source_id
                )
            })
            .map(|tab| tab.id)
        {
            let state = self.log_tab_view_states.entry(tab_id).or_default();
            state.active_search_match = Some(super::ActiveSearchMatch {
                source_id: result.source_id,
                line_number: result.line_number,
                match_ranges: result.match_ranges.clone(),
                active_range: Some(active_range),
            });
        }
    }

    /// 滚动当前日志视图到搜索结果行。
    fn scroll_to_search_result(&mut self, result: &SearchResult) -> bool {
        let Some(tab_id) = self
            .tabs
            .iter()
            .find(|tab| {
                matches!(
                    tab.kind,
                    TabKind::LogSource {
                        source_id,
                        ..
                    } if source_id == result.source_id
                )
            })
            .map(|tab| tab.id)
        else {
            return false;
        };
        let Some(handle) = self.active_log_handle() else {
            return false;
        };
        let is_paged_document = matches!(
            handle.document(),
            crate::reader::log_file_reader::LogDocument::Paged(_)
        );
        let line_count = handle.line_count();
        self.clear_line_marker_jump_cache(tab_id);
        let Some(state) = self.log_tab_view_states.get_mut(&tab_id) else {
            return false;
        };

        if is_paged_document {
            state.paged_scroll.top_px = centered_paged_scroll_top(
                result.line_number,
                line_count,
                f64::from(state.paged_viewport_handle.bounds().size.height),
            );
        } else {
            state
                .scroll_handle
                .scroll_to_item(result.line_number, ScrollStrategy::Center);
        }
        true
    }

    /// 清理所有搜索输入框焦点。
    fn clear_log_search_input_focus(&mut self) {
        self.close_keyword_history();
        self.log_search.keyword_input.is_focused = false;
        self.log_search.keyword_input.marked_range = None;
        self.log_search.keyword_input.selection_drag = None;
        self.log_search.directory_input.is_focused = false;
        self.log_search.directory_input.marked_range = None;
        self.log_search.directory_input.selection_drag = None;
    }

    /// 清理搜索输入历史；重新加载日志来源时调用，避免旧关键字和旧目录污染新来源。
    fn clear_log_search_history_inputs(&mut self) {
        self.clear_quick_log_search_state();
        reset_log_search_input_state(&mut self.log_search.keyword_input);
        reset_log_search_input_state(&mut self.log_search.directory_input);
        self.log_search.directory_source_id = None;
    }

    /// 返回指定搜索输入框状态。
    fn log_search_input(&self, input_kind: LogSearchInputKind) -> &TextInputState {
        match input_kind {
            LogSearchInputKind::Keyword => &self.log_search.keyword_input,
            LogSearchInputKind::Directory => &self.log_search.directory_input,
        }
    }

    /// 返回指定搜索输入框可变状态。
    fn log_search_input_mut(&mut self, input_kind: LogSearchInputKind) -> &mut TextInputState {
        match input_kind {
            LogSearchInputKind::Keyword => &mut self.log_search.keyword_input,
            LogSearchInputKind::Directory => &mut self.log_search.directory_input,
        }
    }

    /// 移动搜索输入框光标。
    fn move_log_search_input_cursor(
        &mut self,
        input_kind: LogSearchInputKind,
        next_cursor: usize,
        should_select: bool,
    ) {
        let input = self.log_search_input_mut(input_kind);
        let previous_cursor = input.cursor;
        let text_length = character_count(&input.value);
        input.cursor = next_cursor.min(text_length);
        input.marked_range = None;
        input.selection_drag = None;

        if should_select {
            if input.selection_anchor.is_none() {
                input.selection_anchor = Some(previous_cursor);
            }
        } else {
            input.selection_anchor = None;
        }
    }

    /// 搜索输入框向左移动。
    fn move_log_search_input_left(&mut self, input_kind: LogSearchInputKind, should_select: bool) {
        if !should_select
            && let Some(selection_range) = self.log_search_input_selection_range(input_kind)
        {
            self.move_log_search_input_cursor(input_kind, selection_range.start, false);
            return;
        }
        let cursor = self.log_search_input(input_kind).cursor.saturating_sub(1);
        self.move_log_search_input_cursor(input_kind, cursor, should_select);
    }

    /// 搜索输入框向右移动。
    fn move_log_search_input_right(&mut self, input_kind: LogSearchInputKind, should_select: bool) {
        if !should_select
            && let Some(selection_range) = self.log_search_input_selection_range(input_kind)
        {
            self.move_log_search_input_cursor(input_kind, selection_range.end, false);
            return;
        }
        let cursor = self.log_search_input(input_kind).cursor + 1;
        self.move_log_search_input_cursor(input_kind, cursor, should_select);
    }

    /// 删除搜索输入框选区。
    fn delete_log_search_input_selection(&mut self, input_kind: LogSearchInputKind) -> bool {
        let Some(selection_range) = self.log_search_input_selection_range(input_kind) else {
            return false;
        };
        let input = self.log_search_input_mut(input_kind);
        input.value = remove_character_range(&input.value, selection_range.clone());
        input.cursor = selection_range.start;
        input.selection_anchor = None;
        input.marked_range = None;
        input.selection_drag = None;
        if input_kind == LogSearchInputKind::Keyword {
            self.clear_quick_log_search_state();
        }
        true
    }

    /// 在搜索输入框插入文本。
    fn insert_log_search_input_text(&mut self, input_kind: LogSearchInputKind, text: &str) {
        self.delete_log_search_input_selection(input_kind);
        let input = self.log_search_input_mut(input_kind);
        input.value = insert_text_at_character_index(&input.value, input.cursor, text);
        input.cursor += character_count(text);
        input.selection_anchor = None;
        input.marked_range = None;
        input.selection_drag = None;
        if input_kind == LogSearchInputKind::Keyword {
            self.clear_quick_log_search_state();
            self.reset_keyword_history_highlight();
        }
    }

    /// 搜索输入框向后删除。
    fn delete_log_search_input_backward(&mut self, input_kind: LogSearchInputKind) {
        if self.delete_log_search_input_selection(input_kind)
            || self.log_search_input(input_kind).cursor == 0
        {
            return;
        }
        let cursor = self.log_search_input(input_kind).cursor;
        let input = self.log_search_input_mut(input_kind);
        input.value = remove_character_range(&input.value, cursor - 1..cursor);
        input.cursor -= 1;
        input.marked_range = None;
        input.selection_drag = None;
        if input_kind == LogSearchInputKind::Keyword {
            self.clear_quick_log_search_state();
        }
    }

    /// 搜索输入框向前删除。
    fn delete_log_search_input_forward(&mut self, input_kind: LogSearchInputKind) {
        if self.delete_log_search_input_selection(input_kind) {
            return;
        }
        let cursor = self.log_search_input(input_kind).cursor;
        let text_length = character_count(&self.log_search_input(input_kind).value);
        if cursor >= text_length {
            return;
        }
        let input = self.log_search_input_mut(input_kind);
        input.value = remove_character_range(&input.value, cursor..cursor + 1);
        input.marked_range = None;
        input.selection_drag = None;
        if input_kind == LogSearchInputKind::Keyword {
            self.clear_quick_log_search_state();
        }
    }

    /// 全选搜索输入框文本。
    fn select_all_log_search_input(&mut self, input_kind: LogSearchInputKind) {
        let input = self.log_search_input_mut(input_kind);
        input.selection_anchor = Some(0);
        input.cursor = character_count(&input.value);
        input.marked_range = None;
        input.selection_drag = None;
    }

    /// 复制搜索输入框选中文本。
    fn copy_log_search_input_selection(
        &mut self,
        input_kind: LogSearchInputKind,
        cx: &mut Context<Self>,
    ) {
        let Some(text) = self.selected_log_search_input_text(input_kind) else {
            return;
        };
        let app_context: &gpui::App = (*cx).borrow();
        app_context.write_to_clipboard(ClipboardItem::new_string(text));
    }

    /// 剪切搜索输入框选中文本。
    fn cut_log_search_input_selection(
        &mut self,
        input_kind: LogSearchInputKind,
        cx: &mut Context<Self>,
    ) {
        self.copy_log_search_input_selection(input_kind, cx);
        self.delete_log_search_input_selection(input_kind);
    }

    /// 粘贴剪贴板文本到搜索输入框。
    fn paste_log_search_input_clipboard(
        &mut self,
        input_kind: LogSearchInputKind,
        cx: &mut Context<Self>,
    ) {
        let app_context: &gpui::App = (*cx).borrow();
        let Some(item) = app_context.read_from_clipboard() else {
            return;
        };
        if let Some(text) = item.text() {
            self.insert_log_search_input_text(input_kind, &text.replace(['\n', '\r'], " "));
        }
    }

    /// 返回搜索输入框当前选中文本。
    fn selected_log_search_input_text(&self, input_kind: LogSearchInputKind) -> Option<String> {
        let selection_range = self.log_search_input_selection_range(input_kind)?;
        Some(slice_character_range(
            &self.log_search_input(input_kind).value,
            selection_range,
        ))
    }
}

/// 启动普通搜索后台线程；调用方需保证目标列表已经准备好。
fn spawn_search_worker(
    request: SearchRequest,
    cancel_token: Arc<AtomicBool>,
    sender: mpsc::Sender<SearchWorkerEvent>,
) {
    std::thread::spawn(move || {
        let progress_sender = sender.clone();
        let result_sender = sender.clone();
        let summary = SearchEngine::search(
            request,
            move |progress| {
                let _ = progress_sender.send(SearchWorkerEvent::Progress(progress));
            },
            move |results| {
                let _ = result_sender.send(SearchWorkerEvent::Results(results));
            },
            cancel_token,
        );
        let _ = sender.send(SearchWorkerEvent::Finished(summary));
    });
}

/// 启动目录搜索后台线程；目录补齐、目标收集和正文扫描全部离开 UI 线程。
fn spawn_directory_search_worker(
    directory_id: SourceId,
    mut registry: SourceRegistry,
    loader_config: crate::config::LoaderConfig,
    request: SearchRequest,
    cancel_token: Arc<AtomicBool>,
    sender: mpsc::Sender<SearchWorkerEvent>,
) {
    std::thread::spawn(move || {
        if registry.node(directory_id).is_none() {
            let _ = sender.send(SearchWorkerEvent::Failed(
                "未在来源树中找到该目录".to_string(),
            ));
            return;
        }

        let loader = LogSourceLoader::new(loader_config)
            .with_archive_passwords(request.archive_passwords.clone())
            .with_deferred_archive_probe();
        let mut pending_ids = vec![directory_id];
        let mut visited_ids = BTreeSet::new();
        let mut target_batch = Vec::with_capacity(DIRECTORY_SEARCH_TARGET_BATCH_SIZE);
        let mut summary = SearchTaskSummary::default();
        let mut errors = Vec::new();
        let mut discovered_total = 0usize;
        let mut last_prepared_total = 0usize;
        let mut registry_dirty = false;

        while let Some(source_id) = pending_ids.pop() {
            if cancel_token.load(Ordering::Relaxed) {
                summary.was_cancelled = true;
                break;
            }
            if !visited_ids.insert(source_id) {
                continue;
            }

            let Some(node) = registry.node(source_id).cloned() else {
                continue;
            };

            if node.kind.can_expand() && !node.metadata.children_loaded && !node.metadata.is_loading
            {
                registry.set_loading(source_id, true);
                let report = loader.load_children(&node);
                errors.extend(report.errors.iter().cloned());
                apply_search_directory_child_report_to_registry(&mut registry, source_id, report);
                registry_dirty = true;
            }

            let child_ids = registry.child_ids(source_id).to_vec();
            for child_id in child_ids.into_iter().rev() {
                if cancel_token.load(Ordering::Relaxed) {
                    summary.was_cancelled = true;
                    break;
                }

                if let Some(target) = search_target_from_registry(&registry, child_id) {
                    target_batch.push(target);
                    discovered_total += 1;

                    if target_batch.len() >= DIRECTORY_SEARCH_TARGET_BATCH_SIZE {
                        send_directory_search_prepared(
                            &sender,
                            registry_dirty.then(|| registry.clone()),
                            discovered_total,
                        );
                        registry_dirty = false;
                        last_prepared_total = discovered_total;

                        let batch_summary = search_discovered_target_batch(
                            &request,
                            &mut target_batch,
                            discovered_total,
                            summary.scanned_files,
                            Arc::clone(&cancel_token),
                            &sender,
                        );
                        let was_cancelled = batch_summary.was_cancelled;
                        merge_search_summary(&mut summary, batch_summary);
                        if was_cancelled {
                            break;
                        }
                    }
                    continue;
                }

                if registry
                    .node(child_id)
                    .is_some_and(|node| node.kind.can_expand())
                {
                    pending_ids.push(child_id);
                }
            }

            if summary.was_cancelled {
                break;
            }
        }

        if !summary.was_cancelled && !target_batch.is_empty() {
            send_directory_search_prepared(
                &sender,
                registry_dirty.then(|| registry.clone()),
                discovered_total,
            );
            registry_dirty = false;
            last_prepared_total = discovered_total;

            let batch_summary = search_discovered_target_batch(
                &request,
                &mut target_batch,
                discovered_total,
                summary.scanned_files,
                Arc::clone(&cancel_token),
                &sender,
            );
            merge_search_summary(&mut summary, batch_summary);
        }

        summary.errors.extend(errors);

        if summary.was_cancelled {
            let _ = sender.send(SearchWorkerEvent::Finished(summary));
            return;
        }

        if discovered_total == 0 {
            let message = if summary.errors.is_empty() {
                "目录下没有日志文件".to_string()
            } else {
                format!("目录搜索目标加载失败：{}", summary.errors.join("；"))
            };
            let _ = sender.send(SearchWorkerEvent::Failed(message));
            return;
        }

        if registry_dirty || last_prepared_total != discovered_total {
            send_directory_search_prepared(
                &sender,
                registry_dirty.then_some(registry),
                discovered_total,
            );
        }

        let _ = sender.send(SearchWorkerEvent::Finished(summary));
    });
}

/// 向 UI 线程报告目录搜索已经发现的目标数量和可选来源树快照。
fn send_directory_search_prepared(
    sender: &mpsc::Sender<SearchWorkerEvent>,
    registry: Option<SourceRegistry>,
    total_files: usize,
) {
    let _ = sender.send(SearchWorkerEvent::Prepared(Box::new(SearchPreparedEvent {
        registry,
        total_files,
    })));
}

/// 搜索当前已经发现的一批目录目标，并把批内进度映射成目录总进度。
fn search_discovered_target_batch(
    request_template: &SearchRequest,
    target_batch: &mut Vec<SearchTarget>,
    discovered_total: usize,
    scanned_files_base: usize,
    cancel_token: Arc<AtomicBool>,
    sender: &mpsc::Sender<SearchWorkerEvent>,
) -> SearchTaskSummary {
    let mut request = request_template.clone();
    request.targets = std::mem::take(target_batch);

    let progress_sender = sender.clone();
    let result_sender = sender.clone();
    SearchEngine::search(
        request,
        move |mut progress| {
            progress.scanned_files += scanned_files_base;
            progress.total_files = discovered_total;
            let _ = progress_sender.send(SearchWorkerEvent::Progress(progress));
        },
        move |results| {
            let _ = result_sender.send(SearchWorkerEvent::Results(results));
        },
        cancel_token,
    )
}

/// 合并分批目录搜索的摘要数据。
fn merge_search_summary(total: &mut SearchTaskSummary, batch: SearchTaskSummary) {
    total.was_cancelled |= batch.was_cancelled;
    total.scanned_files += batch.scanned_files;
    total.scanned_lines += batch.scanned_lines;
    total.scanned_bytes = total.scanned_bytes.saturating_add(batch.scanned_bytes);
    total.matched_results += batch.matched_results;
    total.errors.extend(batch.errors);
}

/// 在后台补齐目录搜索所需的来源树，并收集可搜索日志目标。
#[cfg(test)]
fn prepare_directory_search_targets(
    mut registry: SourceRegistry,
    directory_id: SourceId,
    loader_config: crate::config::LoaderConfig,
) -> Result<DirectorySearchPreparation, String> {
    if registry.node(directory_id).is_none() {
        return Err("未在来源树中找到该目录".to_string());
    }

    let loader = LogSourceLoader::new(loader_config);
    let mut pending_ids = vec![directory_id];
    let mut visited_ids = BTreeSet::new();
    let mut errors = Vec::new();

    while let Some(source_id) = pending_ids.pop() {
        if !visited_ids.insert(source_id) {
            continue;
        }

        let Some(node) = registry.node(source_id).cloned() else {
            continue;
        };

        if node.kind.can_expand() && !node.metadata.children_loaded && !node.metadata.is_loading {
            registry.set_loading(source_id, true);
            let report = loader.load_children(&node);
            errors.extend(report.errors.iter().cloned());
            apply_search_directory_child_report_to_registry(&mut registry, source_id, report);
        }

        let child_ids = registry.child_ids(source_id).to_vec();
        for child_id in child_ids.into_iter().rev() {
            pending_ids.push(child_id);
        }
    }

    let targets = collect_loaded_log_targets_under_registry(&registry, directory_id);
    Ok(DirectorySearchPreparation {
        registry,
        targets,
        errors,
    })
}

/// 将后台目录补齐得到的子级注册表挂回来源树快照。
fn apply_search_directory_child_report_to_registry(
    registry: &mut SourceRegistry,
    parent_id: SourceId,
    report: LoadReport,
) {
    if report.registry.is_empty() {
        if let Some(parent) = registry.node_mut(parent_id) {
            parent.metadata.is_loading = false;
            parent.metadata.children_loaded = report.errors.is_empty();
            parent.metadata.message = if report.errors.is_empty() {
                Some("没有可显示的子节点".to_string())
            } else {
                Some(report.errors.join("；"))
            };
        }
        registry.rebuild_all_indices();
        return;
    }

    let should_keep_expanded = registry
        .node(parent_id)
        .map(|node| node.expanded)
        .unwrap_or(false);
    registry.append_children_registry(parent_id, report.registry, should_keep_expanded);

    if let Some(parent) = registry.node_mut(parent_id)
        && !report.errors.is_empty()
    {
        parent.metadata.message = Some(report.errors.join("；"));
    }
}

/// 从指定来源树快照目录下递归收集日志候选。
#[cfg(test)]
fn collect_loaded_log_targets_under_registry(
    registry: &SourceRegistry,
    directory_id: SourceId,
) -> Vec<SearchTarget> {
    let mut targets = Vec::new();
    collect_loaded_log_targets_recursive(registry, directory_id, &mut targets);
    targets
}

/// 递归收集来源树快照中的日志候选。
#[cfg(test)]
fn collect_loaded_log_targets_recursive(
    registry: &SourceRegistry,
    parent_id: SourceId,
    targets: &mut Vec<SearchTarget>,
) {
    for child_id in registry.child_ids(parent_id).iter().copied() {
        if let Some(target) = search_target_from_registry(registry, child_id) {
            targets.push(target);
            continue;
        }

        if registry
            .node(child_id)
            .is_some_and(|node| node.kind.can_expand())
        {
            collect_loaded_log_targets_recursive(registry, child_id, targets);
        }
    }
}

/// 将来源树快照中的日志节点转换为搜索目标。
fn search_target_from_registry(
    registry: &SourceRegistry,
    source_id: SourceId,
) -> Option<SearchTarget> {
    let node = registry.node(source_id)?;
    if !node.kind.is_log_candidate() {
        return None;
    }

    Some(SearchTarget {
        source_id,
        label: node.label.clone(),
        path: node.location.display_path(),
        location: node.location.clone(),
    })
}

/// 估算搜索结果行宽度；只影响横向滚动范围，不影响真实结果内容和定位。
fn estimated_search_result_row_width(result: &SearchResult) -> f32 {
    let preview_width = result
        .line_text
        .chars()
        .take(SEARCH_RESULT_PREVIEW_ESTIMATE_CHARS)
        .map(|character| {
            if character.is_ascii() {
                SEARCH_RESULT_ASCII_CHAR_WIDTH_ESTIMATE
            } else {
                SEARCH_RESULT_WIDE_CHAR_WIDTH_ESTIMATE
            }
        })
        .sum::<f32>();
    let metadata_width = 112.0;
    (metadata_width + preview_width + 64.0).max(SEARCH_RESULT_LIST_MIN_WIDTH)
}

/// 将“第 N 次出现”映射为对应结果行和单个命中范围。
fn quick_match_result_for_occurrence(
    matches: &[SearchResult],
    occurrence_index: usize,
) -> Option<(SearchResult, Range<usize>)> {
    let mut remaining = occurrence_index;
    for result in matches {
        if remaining < result.match_ranges.len() {
            return Some((result.clone(), result.match_ranges[remaining].clone()));
        }
        remaining = remaining.saturating_sub(result.match_ranges.len());
    }
    None
}

/// 将搜索输入框恢复为空闲空值；用于“重新加载日志”这类需要清空搜索记录的场景。
fn reset_log_search_input_state(input: &mut TextInputState) {
    input.value.clear();
    input.cursor = 0;
    input.selection_anchor = None;
    input.marked_range = None;
    input.selection_drag = None;
    input.is_focused = false;
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::app::{ArgusTab, LogTextPosition, LogTextSelection, SEARCH_RESULT_PANEL_HEIGHT_MAX};
    use crate::config::paths::{isolated_test_dir, isolated_test_file_path};
    use crate::config::{ConfigManager, LoaderConfig, SEARCH_RECENT_KEYWORDS_MAX};
    use crate::loader::archive::ArchivePasswordStore;
    use crate::loader::{
        LogSourceLoader, SourceKind, SourceLocation, SourceMetadata, SourceRegistry, SourceTreeNode,
    };
    use crate::reader::log_file_reader::{LogFileReader, LogOpenState, OpenLogRequest};

    /// 构造隔离配置路径的应用状态。
    fn test_app() -> ArgusApp {
        let config_dir = isolated_test_dir("log-search");
        ArgusApp::new_with_config_manager(ConfigManager::new(config_dir.join("settings.toml")))
    }

    #[test]
    fn paged_search_jump_centers_result_line() {
        let row_height = LOG_VIEWER_ROW_HEIGHT as f64;

        assert_eq!(centered_paged_scroll_top(0, 200, row_height * 20.0), 0.0);
        assert_eq!(
            centered_paged_scroll_top(50, 200, row_height * 20.0),
            row_height * 40.5
        );
        assert_eq!(
            centered_paged_scroll_top(199, 200, row_height * 20.0),
            row_height * 180.0
        );
    }

    /// 验证快搜关键字解析会 trim、过滤空项并按首次出现去重。
    #[test]
    fn quick_search_keywords_are_trimmed_and_deduplicated() {
        assert_eq!(
            parse_quick_search_keywords(" ERROR, WARN,,ERROR, timeout ,WARN "),
            vec![
                "ERROR".to_string(),
                "WARN".to_string(),
                "timeout".to_string()
            ]
        );
    }

    /// 构造包含一个目录和两个日志文件的来源树。
    fn registry_with_directory_logs() -> (SourceRegistry, SourceId, SourceId, SourceId) {
        let mut registry = SourceRegistry::new();
        let root_id = registry.allocate_id();
        registry.insert_node(SourceTreeNode {
            id: root_id,
            parent_id: None,
            depth: 0,
            label: "logs".to_string(),
            kind: SourceKind::Directory,
            location: SourceLocation::LocalPath(PathBuf::from("logs")),
            metadata: SourceMetadata {
                children_loaded: true,
                ..SourceMetadata::default()
            },
            selected: false,
            expanded: true,
        });

        let first_id = registry.allocate_id();
        registry.insert_node(SourceTreeNode {
            id: first_id,
            parent_id: Some(root_id),
            depth: 1,
            label: "app.log".to_string(),
            kind: SourceKind::LogFile,
            location: SourceLocation::LocalPath(PathBuf::from("logs/app.log")),
            metadata: SourceMetadata::default(),
            selected: false,
            expanded: false,
        });

        let second_id = registry.allocate_id();
        registry.insert_node(SourceTreeNode {
            id: second_id,
            parent_id: Some(root_id),
            depth: 1,
            label: "error.log".to_string(),
            kind: SourceKind::LogFile,
            location: SourceLocation::LocalPath(PathBuf::from("logs/error.log")),
            metadata: SourceMetadata::default(),
            selected: false,
            expanded: false,
        });
        registry.rebuild_all_indices();

        (registry, root_id, first_id, second_id)
    }

    /// 构造包含三个日志文件的来源树，用于验证连续 Shift 扩展选择。
    fn registry_with_three_directory_logs() -> (SourceRegistry, SourceId, SourceId, SourceId) {
        let (mut registry, root_id, first_id, second_id) = registry_with_directory_logs();
        let third_id = registry.allocate_id();
        registry.insert_node(SourceTreeNode {
            id: third_id,
            parent_id: Some(root_id),
            depth: 1,
            label: "access.log".to_string(),
            kind: SourceKind::LogFile,
            location: SourceLocation::LocalPath(PathBuf::from("logs/access.log")),
            metadata: SourceMetadata::default(),
            selected: false,
            expanded: false,
        });
        registry.rebuild_all_indices();

        (registry, first_id, second_id, third_id)
    }

    /// 构造搜索结果样本，便于验证旧 generation 事件不会污染当前面板。
    fn sample_search_result(source_id: SourceId) -> SearchResult {
        SearchResult {
            source_id,
            label: "app.log".to_string(),
            path: "logs/app.log".to_string(),
            line_number: 1,
            line_text: "ERROR".to_string(),
            match_ranges: std::iter::once(0..5).collect(),
            matched_keywords: vec!["ERROR".to_string()],
        }
    }

    /// 验证目录搜索会收集该目录下已加载的所有日志候选。
    #[test]
    fn directory_scope_collects_loaded_log_targets() {
        let (registry, root_id, first_id, second_id) = registry_with_directory_logs();
        let mut app = test_app();
        app.source_registry = registry;
        app.log_search.directory_input.value = "logs".to_string();

        let targets = app
            .search_targets_for_scope(SearchScope::Directory)
            .unwrap();

        assert_eq!(app.resolve_search_directory_source_id().unwrap(), root_id);
        assert_eq!(
            targets
                .iter()
                .map(|target| target.source_id)
                .collect::<Vec<_>>(),
            vec![first_id, second_id]
        );
    }

    /// 验证目录搜索准备会在后台来源树快照中补齐未展开子目录，避免漏搜懒加载节点。
    #[test]
    fn directory_search_preparation_loads_unloaded_children_before_collecting_targets() {
        let root = isolated_test_dir("log-search-lazy-directory");
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("lazy.log"), "INFO lazy child").unwrap();

        let report = LogSourceLoader::new(LoaderConfig::default()).load_paths(vec![root.clone()]);
        assert!(report.errors.is_empty());
        let mut app = test_app();
        app.source_registry = report.registry;
        app.log_search.directory_input.value = root.display().to_string();

        let directory_id = app.resolve_search_directory_source_id().unwrap();
        let preparation = prepare_directory_search_targets(
            app.source_registry.clone(),
            directory_id,
            app.config.loader.clone(),
        )
        .unwrap();

        assert!(preparation.errors.is_empty());
        assert!(
            preparation
                .targets
                .iter()
                .any(|target| target.label == "lazy.log")
        );
        let lazy_parent_loaded = preparation
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|source_id| preparation.registry.node(*source_id))
            .find(|node| node.label == "nested")
            .is_some_and(|node| node.metadata.children_loaded);
        assert!(lazy_parent_loaded);
        let original_lazy_parent_loaded = app
            .source_registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|source_id| app.source_registry.node(*source_id))
            .find(|node| node.label == "nested")
            .is_some_and(|node| node.metadata.children_loaded);
        assert!(!original_lazy_parent_loaded);

        let _ = std::fs::remove_dir_all(root);
    }

    /// 验证搜索运行期重置会取消旧任务引用并清理旧来源选择。
    #[test]
    fn reset_search_runtime_state_clears_results_and_selected_sources() {
        let (_registry, _root_id, first_id, _second_id) = registry_with_directory_logs();
        let mut app = test_app();
        app.selected_search_source_ids.insert(first_id);
        app.log_search.keyword_input.value = "ERROR".to_string();
        app.log_search.directory_input.value = "logs".to_string();
        app.log_search.directory_source_id = Some(first_id);
        app.log_search.results.push(SearchResult {
            source_id: first_id,
            label: "app.log".to_string(),
            path: "logs/app.log".to_string(),
            line_number: 1,
            line_text: "ERROR".to_string(),
            match_ranges: std::iter::once(0..5).collect(),
            matched_keywords: vec!["ERROR".to_string()],
        });

        app.reset_log_search_runtime_state();

        assert!(app.selected_search_source_ids.is_empty());
        assert!(app.log_search.results.is_empty());
        assert!(app.log_search.keyword_input.value.is_empty());
        assert!(app.log_search.directory_input.value.is_empty());
        assert_eq!(app.log_search.directory_source_id, None);
        assert_eq!(app.log_search.task_state, SearchTaskState::Idle);
    }

    /// 验证取消搜索会推进 generation，旧后台结果不能继续追加到搜索面板。
    #[test]
    fn cancelling_search_invalidates_stale_worker_events() {
        let (_registry, _root_id, first_id, _second_id) = registry_with_directory_logs();
        let mut app = test_app();
        let old_generation = 7;
        let cancel_token = Arc::new(AtomicBool::new(false));
        app.log_search.generation = old_generation;
        app.log_search.task_state = SearchTaskState::Running;
        app.log_search.cancel_token = Some(cancel_token.clone());
        app.log_search.progress.total_files = 3;

        app.cancel_log_search();
        app.apply_search_progress(
            old_generation,
            SearchProgress {
                scanned_files: 2,
                total_files: 3,
                ..SearchProgress::default()
            },
        );
        app.append_search_results(old_generation, vec![sample_search_result(first_id)]);

        assert!(cancel_token.load(Ordering::Relaxed));
        assert_eq!(app.log_search.generation, old_generation + 1);
        assert_eq!(app.log_search.task_state, SearchTaskState::Cancelled);
        assert_eq!(app.log_search.progress.scanned_files, 0);
        assert!(app.log_search.results.is_empty());
    }

    /// 验证后台线程异常断开时会收敛为失败状态，避免搜索按钮长期停在取消态。
    #[test]
    fn disconnected_search_worker_does_not_leave_task_running() {
        let mut app = test_app();
        let generation = 3;
        app.log_search.generation = generation;
        app.log_search.task_state = SearchTaskState::Running;
        app.log_search.cancel_token = Some(Arc::new(AtomicBool::new(false)));

        app.mark_search_worker_disconnected(generation - 1);
        assert_eq!(app.log_search.task_state, SearchTaskState::Running);

        app.mark_search_worker_disconnected(generation);

        assert!(matches!(
            app.log_search.task_state,
            SearchTaskState::Failed(_)
        ));
        assert!(app.log_search.cancel_token.is_none());
        assert_eq!(
            app.log_search.message.as_deref(),
            Some("搜索任务已中断，请重试")
        );
    }

    /// 验证关闭结果面板会取消搜索任务并让旧任务事件失效。
    #[test]
    fn closing_search_results_panel_cancels_running_search() {
        let (_registry, _root_id, first_id, _second_id) = registry_with_directory_logs();
        let mut app = test_app();
        let old_generation = 11;
        let cancel_token = Arc::new(AtomicBool::new(false));
        app.log_search.generation = old_generation;
        app.log_search.task_state = SearchTaskState::Running;
        app.log_search.cancel_token = Some(cancel_token.clone());
        app.log_search.results.push(sample_search_result(first_id));

        app.close_log_search_results_panel();
        app.append_search_results(old_generation, vec![sample_search_result(first_id)]);

        assert!(cancel_token.load(Ordering::Relaxed));
        assert_eq!(app.log_search.generation, old_generation + 1);
        assert_eq!(app.log_search.task_state, SearchTaskState::Idle);
        assert!(app.log_search.results.is_empty());
    }

    /// 验证唤起搜索窗口时会保留上次关键字并自动全选，方便直接覆盖输入。
    #[test]
    fn opening_search_window_selects_existing_keyword() {
        let mut app = test_app();
        app.log_search.keyword_input.value = "ERROR".to_string();

        app.prepare_log_search_defaults();
        app.focus_log_search_keyword_for_open();

        assert_eq!(app.log_search.keyword_input.cursor, 5);
        assert_eq!(app.log_search.keyword_input.selection_anchor, Some(0));
        assert!(app.log_search.keyword_input.is_focused);
    }

    /// 验证日志选区存在时会优先填充到搜索关键字，覆盖上一次搜索词。
    #[test]
    fn selected_log_text_prefills_search_keyword() {
        let (_registry, _root_id, first_id, _second_id) = registry_with_directory_logs();
        let mut app = test_app();
        let log_path = isolated_test_file_path("log-search-selected-keyword", "selected.log");
        std::fs::write(&log_path, "first ERROR line\nsecond line").unwrap();
        let handle = LogFileReader::open(OpenLogRequest {
            location: SourceLocation::LocalPath(log_path.clone()),
            label: "app.log".to_string(),
            default_encoding: "utf-8".to_string(),
            archive_passwords: ArchivePasswordStore::default(),
        })
        .unwrap();

        app.tabs = vec![ArgusTab {
            id: 1,
            title: "app.log".to_string(),
            kind: TabKind::LogSource {
                source_id: first_id,
                path: log_path.display().to_string(),
            },
        }];
        app.active_tab_id = 1;
        app.ensure_log_tab_view_state(1);
        app.log_read_states
            .insert(first_id, LogOpenState::Ready(handle));
        app.log_tab_view_states.get_mut(&1).unwrap().selection = Some(LogTextSelection {
            anchor: LogTextPosition {
                line_index: 0,
                column: 6,
            },
            focus: LogTextPosition {
                line_index: 0,
                column: 11,
            },
        });
        app.log_search.keyword_input.value = "WARN".to_string();

        app.prepare_log_search_defaults();

        assert_eq!(app.log_search.keyword_input.value, "ERROR");
        let _ = std::fs::remove_file(log_path);
    }

    /// 验证搜索窗口选项切换更新真实搜索状态，而不是旧占位搜索字段。
    #[test]
    fn toggles_log_search_options_on_runtime_state() {
        let mut app = test_app();

        app.toggle_log_search_case_sensitive();
        app.toggle_log_search_regex_enabled();

        assert!(app.log_search.case_sensitive);
        assert!(app.log_search.regex_enabled);
    }

    /// 验证同一行多个命中时，快速定位可以高亮具体的单个命中范围。
    #[test]
    fn quick_match_activation_tracks_single_occurrence_range() {
        let (_registry, _root_id, first_id, _second_id) = registry_with_directory_logs();
        let mut app = test_app();
        app.tabs = vec![ArgusTab {
            id: 1,
            title: "app.log".to_string(),
            kind: TabKind::LogSource {
                source_id: first_id,
                path: "logs/app.log".to_string(),
            },
        }];
        app.active_tab_id = 1;
        app.ensure_log_tab_view_state(1);
        app.log_search.quick_match_count = 3;
        app.log_search.quick_matches = vec![
            SearchResult {
                source_id: first_id,
                label: "app.log".to_string(),
                path: "logs/app.log".to_string(),
                line_number: 0,
                line_text: "ERROR ERROR".to_string(),
                match_ranges: vec![0..5, 6..11],
                matched_keywords: vec!["ERROR".to_string()],
            },
            SearchResult {
                source_id: first_id,
                label: "app.log".to_string(),
                path: "logs/app.log".to_string(),
                line_number: 2,
                line_text: "ERROR".to_string(),
                match_ranges: std::iter::once(0..5).collect(),
                matched_keywords: vec!["ERROR".to_string()],
            },
        ];

        app.activate_quick_match_at_index(1);

        let active_match = app
            .log_tab_view_state(1)
            .and_then(|state| state.active_search_match.as_ref())
            .unwrap();
        assert_eq!(app.log_search.active_quick_match_index, Some(1));
        assert_eq!(active_match.line_number, 0);
        assert_eq!(active_match.match_ranges, vec![0..5, 6..11]);
        assert_eq!(active_match.active_range, Some(6..11));
    }

    /// 验证关键字变化后会清理当前日志快速查找缓存。
    #[test]
    fn keyword_edit_clears_quick_match_cache() {
        let mut app = test_app();
        app.log_search.keyword_input.value = "ERROR".to_string();
        app.log_search.keyword_input.cursor = 5;
        app.log_search.quick_match_count = 1;
        app.log_search.quick_matches.push(SearchResult {
            source_id: SourceId(1),
            label: "app.log".to_string(),
            path: "logs/app.log".to_string(),
            line_number: 0,
            line_text: "ERROR".to_string(),
            match_ranges: std::iter::once(0..5).collect(),
            matched_keywords: vec!["ERROR".to_string()],
        });

        app.insert_log_search_input_text(LogSearchInputKind::Keyword, "!");

        assert_eq!(app.log_search.keyword_input.value, "ERROR!");
        assert_eq!(app.log_search.quick_match_count, 0);
        assert!(app.log_search.quick_matches.is_empty());
        assert_eq!(app.log_search.active_quick_match_index, None);
    }

    /// 验证搜索结果追加时会按文件生成分组，折叠后只保留分组标题行。
    #[test]
    fn search_results_are_grouped_and_collapsible() {
        let (_registry, _root_id, first_id, second_id) = registry_with_directory_logs();
        let mut app = test_app();

        app.append_search_results(
            app.log_search.generation,
            vec![
                SearchResult {
                    source_id: first_id,
                    label: "app.log".to_string(),
                    path: "logs/app.log".to_string(),
                    line_number: 1,
                    line_text: "ERROR one".to_string(),
                    match_ranges: std::iter::once(0..5).collect(),
                    matched_keywords: vec!["ERROR".to_string()],
                },
                SearchResult {
                    source_id: first_id,
                    label: "app.log".to_string(),
                    path: "logs/app.log".to_string(),
                    line_number: 2,
                    line_text: "ERROR two".to_string(),
                    match_ranges: std::iter::once(0..5).collect(),
                    matched_keywords: vec!["ERROR".to_string()],
                },
                SearchResult {
                    source_id: second_id,
                    label: "error.log".to_string(),
                    path: "logs/error.log".to_string(),
                    line_number: 3,
                    line_text: "ERROR three".to_string(),
                    match_ranges: std::iter::once(0..5).collect(),
                    matched_keywords: vec!["ERROR".to_string()],
                },
            ],
        );

        assert_eq!(app.log_search.result_groups.len(), 2);
        assert_eq!(app.log_search.visible_result_items.len(), 5);

        app.toggle_search_result_group(0);

        assert_eq!(
            app.log_search.visible_result_items,
            vec![
                SearchResultListItem::Group(0),
                SearchResultListItem::Group(1),
                SearchResultListItem::Result(2),
            ]
        );

        app.collapse_all_search_result_groups();
        assert_eq!(
            app.log_search.visible_result_items,
            vec![
                SearchResultListItem::Group(0),
                SearchResultListItem::Group(1),
            ]
        );

        app.expand_all_search_result_groups();
        assert_eq!(app.log_search.visible_result_items.len(), 5);
    }

    /// 验证搜索结果面板拖拽高度会被限制在合理范围内。
    #[test]
    fn search_result_panel_resize_is_clamped() {
        let mut app = test_app();

        app.begin_search_result_panel_resize(gpui::px(300.0));
        assert!(app.resize_search_result_panel(gpui::px(-500.0), SEARCH_RESULT_PANEL_HEIGHT_MAX));
        assert_eq!(
            app.log_search.result_panel_height,
            SEARCH_RESULT_PANEL_HEIGHT_MAX
        );

        app.begin_search_result_panel_resize(gpui::px(300.0));
        assert!(app.resize_search_result_panel(gpui::px(900.0), SEARCH_RESULT_PANEL_HEIGHT_MAX));
        assert_eq!(
            app.log_search.result_panel_height,
            SEARCH_RESULT_PANEL_HEIGHT_MIN
        );
    }

    /// 验证连续 Shift 范围选择保留初始锚点，不会把前一次选区意外取消。
    #[test]
    fn shift_range_selection_keeps_original_anchor_when_extending() {
        let (registry, first_id, second_id, third_id) = registry_with_three_directory_logs();
        let mut app = test_app();
        app.source_registry = registry;
        app.last_source_selection_anchor = Some(first_id);

        app.select_source_tree_range_for_search(second_id);
        assert_eq!(
            app.selected_search_source_ids,
            BTreeSet::from([first_id, second_id])
        );

        app.select_source_tree_range_for_search(third_id);
        assert_eq!(
            app.selected_search_source_ids,
            BTreeSet::from([first_id, second_id, third_id])
        );
        assert_eq!(app.last_source_selection_anchor, Some(first_id));
    }

    /// 验证关键字历史去重、置顶、截断到上限，并确认持久化写入配置文件后可原样读回。
    #[test]
    fn keyword_history_dedup_unshift_truncate_and_persist() {
        let mut app = test_app();
        app.record_search_keyword("error");
        assert_eq!(
            app.config.log_search.recent_keywords,
            vec!["error".to_string()]
        );

        // 新关键字置顶。
        app.record_search_keyword("timeout");
        assert_eq!(
            app.config.log_search.recent_keywords,
            vec!["timeout".to_string(), "error".to_string()]
        );

        // 重复关键字去重并重新置顶，保持相对顺序。
        app.record_search_keyword("error");
        assert_eq!(
            app.config.log_search.recent_keywords,
            vec!["error".to_string(), "timeout".to_string()]
        );

        // 空白关键字忽略。
        app.record_search_keyword("   ");
        assert_eq!(
            app.config.log_search.recent_keywords,
            vec!["error".to_string(), "timeout".to_string()]
        );

        // 超过上限时丢弃最旧项，最新项置顶。
        for index in 0..(SEARCH_RECENT_KEYWORDS_MAX + 3) {
            app.record_search_keyword(&format!("kw-{index}"));
        }
        assert_eq!(
            app.config.log_search.recent_keywords.len(),
            SEARCH_RECENT_KEYWORDS_MAX
        );
        let expected_first = format!("kw-{}", SEARCH_RECENT_KEYWORDS_MAX + 2);
        assert_eq!(
            app.config
                .log_search
                .recent_keywords
                .first()
                .map(String::as_str),
            Some(expected_first.as_str())
        );

        // 持久化：重新加载同一配置文件应得到相同历史。
        let reloaded = ConfigManager::new(app.config_manager.settings_path().to_path_buf()).load();
        assert_eq!(
            reloaded.log_search.recent_keywords,
            app.config.log_search.recent_keywords
        );
    }

    /// 验证历史过滤为大小写不敏感的子串匹配，空输入返回全部（最新在前）。
    #[test]
    fn keyword_history_items_returns_all_recent() {
        let mut app = test_app();
        app.record_search_keyword("ERROR");
        app.record_search_keyword("ConnectionTimeout");
        app.record_search_keyword("warn");
        app.record_search_keyword("disk full");

        // 始终返回全部历史，最新在前；输入框残留关键字不再过滤（否则下拉只剩当前关键字）。
        app.log_search.keyword_input.value = String::new();
        assert_eq!(
            app.keyword_history_items(),
            vec![
                "disk full".to_string(),
                "warn".to_string(),
                "ConnectionTimeout".to_string(),
                "ERROR".to_string(),
            ]
        );

        // 输入框已有上次搜索的关键字时，仍返回全部历史。
        app.log_search.keyword_input.value = "ERROR".to_string();
        assert_eq!(
            app.keyword_history_items(),
            vec![
                "disk full".to_string(),
                "warn".to_string(),
                "ConnectionTimeout".to_string(),
                "ERROR".to_string(),
            ]
        );

        // 纯空白输入同样返回全部。
        app.log_search.keyword_input.value = "   ".to_string();
        assert_eq!(app.keyword_history_items().len(), 4);
    }

    /// 验证历史下拉高亮在列表内循环移动，关闭后不再响应。
    #[test]
    fn keyword_history_highlight_cycles_within_list() {
        let mut app = test_app();
        app.record_search_keyword("error");
        app.record_search_keyword("timeout");
        app.record_search_keyword("warn");

        app.log_search.keyword_input.value = String::new();
        app.open_keyword_history();
        assert!(app.log_search.keyword_history_open);
        assert_eq!(app.log_search.keyword_history_highlight, None);

        // 向下：None -> 0 -> 1 -> 2 -> 0（循环）。
        app.move_keyword_history_highlight(1);
        assert_eq!(app.log_search.keyword_history_highlight, Some(0));
        app.move_keyword_history_highlight(1);
        assert_eq!(app.log_search.keyword_history_highlight, Some(1));
        app.move_keyword_history_highlight(1);
        assert_eq!(app.log_search.keyword_history_highlight, Some(2));
        app.move_keyword_history_highlight(1);
        assert_eq!(app.log_search.keyword_history_highlight, Some(0));

        // 向上从 0 循环回末尾。
        app.move_keyword_history_highlight(-1);
        assert_eq!(app.log_search.keyword_history_highlight, Some(2));

        // 输入框残留关键字不影响列表长度，高亮仍按完整列表循环。
        app.log_search.keyword_input.value = "err".to_string();
        app.log_search.keyword_history_highlight = None;
        app.move_keyword_history_highlight(1);
        assert_eq!(app.log_search.keyword_history_highlight, Some(0));
        app.move_keyword_history_highlight(-1);
        assert_eq!(app.log_search.keyword_history_highlight, Some(2));

        // 关闭后清空高亮且不再响应移动。
        app.close_keyword_history();
        assert!(!app.log_search.keyword_history_open);
        assert_eq!(app.log_search.keyword_history_highlight, None);
        app.move_keyword_history_highlight(1);
        assert_eq!(app.log_search.keyword_history_highlight, None);
    }
}
