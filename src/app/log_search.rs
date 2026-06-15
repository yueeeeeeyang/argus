//! 文件职责：维护日志搜索窗口、后台搜索任务、来源树多选和结果跳转逻辑。
//! 创建日期：2026-06-11
//! 修改日期：2026-06-11
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
    ArgusApp, InputTextSelectionDrag, LogSearchInputKind, LogSearchInputState, QuickMatchKey,
    SEARCH_RESULT_PANEL_HEIGHT_MAX, SEARCH_RESULT_PANEL_HEIGHT_MIN, SearchResultGroup,
    SearchResultListItem, SearchResultPanelResizeDrag, TabKind,
};
use crate::app::LOG_VIEWER_ROW_HEIGHT;
use crate::loader::{LoadReport, LogSourceLoader, SourceId, SourceKind, SourceRegistry};
use crate::search::search_engine::{
    CurrentLogMatchCount, CurrentLogMatchDirection, CurrentLogMatchNavigation,
    CurrentLogMatchPosition, SearchEngine, SearchProgress, SearchQuery, SearchRequest,
    SearchResult, SearchScope, SearchTarget, SearchTaskSummary,
};
use crate::search::search_task::SearchTaskState;
use crate::text_selection::{
    TextSelectionGranularity, character_count, insert_text_at_character_index,
    remove_character_range, slice_character_range, word_range_at,
};
use crate::ui::log_search_window::LogSearchWindow;

/// 搜索窗口默认宽度。
const LOG_SEARCH_WINDOW_WIDTH: f32 = 560.0;
/// 搜索窗口默认高度。
const LOG_SEARCH_WINDOW_HEIGHT: f32 = 220.0;
/// 搜索窗口最小宽度。
const LOG_SEARCH_WINDOW_MIN_WIDTH: f32 = 460.0;
/// 搜索窗口最小高度。
const LOG_SEARCH_WINDOW_MIN_HEIGHT: f32 = 190.0;
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
    /// 目录搜索目标已在后台补齐并准备完成。
    Prepared(SearchPreparedEvent),
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
    /// 补齐懒加载节点后的来源树；非目录搜索无需更新。
    registry: Option<SourceRegistry>,
    /// 本次搜索最终目标文件数量。
    total_files: usize,
}

/// 后台目录搜索准备结果。
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
    pub fn open_log_search_window(&mut self, cx: &mut Context<Self>) {
        if !self.ensure_active_log_tab_for_search() {
            self.placeholder_notice = "请先打开日志再搜索".to_string();
            return;
        }

        if self.log_search.is_window_open {
            if let Some(window_handle) = self.log_search.window_handle.clone()
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
    pub fn close_log_search_window(&mut self) {
        self.log_search.is_window_open = false;
        self.log_search.window_handle = None;
        self.clear_log_search_input_focus();
        self.placeholder_notice = "已关闭日志搜索窗口".to_string();
    }

    /// 设置搜索范围。
    pub fn set_log_search_scope(&mut self, scope: SearchScope) {
        self.log_search.scope = scope;
        self.prepare_log_search_directory_default();
        self.placeholder_notice = format!("搜索范围已切换为{}", scope.label());
    }

    /// 切换搜索大小写敏感选项；只影响下一次启动的搜索任务。
    pub fn toggle_log_search_case_sensitive(&mut self) {
        self.log_search.case_sensitive = !self.log_search.case_sensitive;
        self.clear_quick_log_search_state();
        self.placeholder_notice = if self.log_search.case_sensitive {
            "搜索已启用区分大小写".to_string()
        } else {
            "搜索已关闭区分大小写".to_string()
        };
    }

    /// 切换正则搜索选项；关键字保持原样，由启动搜索时统一校验。
    pub fn toggle_log_search_regex_enabled(&mut self) {
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
    pub fn start_log_search(&mut self, scope: SearchScope, cx: &mut Context<Self>) {
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
        self.log_search.generation = self.log_search.generation.wrapping_add(1);
        self.log_search.progress = SearchProgress {
            total_files: targets.len(),
            ..SearchProgress::default()
        };
        self.log_search.task_state = SearchTaskState::Running;
        self.log_search.results.clear();
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
        let (sender, receiver) = mpsc::channel::<SearchWorkerEvent>();

        if let Some((directory_id, registry, loader_config)) = directory_prepare {
            self.log_search.progress.current_path = Some("正在准备目录搜索目标".to_string());
            spawn_directory_search_worker(
                directory_id,
                registry,
                loader_config,
                SearchRequest {
                    generation,
                    scope,
                    query,
                    targets: Vec::new(),
                    default_encoding,
                },
                cancel_token,
                sender,
            );
        } else {
            spawn_search_worker(
                SearchRequest {
                    generation,
                    scope,
                    query,
                    targets,
                    default_encoding,
                },
                cancel_token,
                sender,
            );
        }

        Self::poll_log_search_worker_events(generation, receiver, cx);

        self.placeholder_notice = format!(
            "{}",
            if scope == SearchScope::Directory {
                "正在准备目录搜索目标".to_string()
            } else {
                format!(
                    "正在搜索 {} 个日志文件",
                    self.log_search.progress.total_files
                )
            }
        );
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
                let mut prepared_event = None;
                let mut latest_progress = None;
                let mut pending_results = Vec::new();
                let mut failed_message = None;
                let mut finished_summary = None;
                let mut should_continue = true;

                for _ in 0..LOG_SEARCH_MAX_EVENTS_PER_TICK {
                    match receiver.try_recv() {
                        Ok(SearchWorkerEvent::Prepared(event)) => {
                            prepared_event = Some(event);
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
                            break;
                        }
                    }
                }

                if prepared_event.is_some()
                    || latest_progress.is_some()
                    || !pending_results.is_empty()
                    || failed_message.is_some()
                    || finished_summary.is_some()
                {
                    view.update(cx, |app, cx| {
                        if let Some(event) = prepared_event {
                            app.apply_search_worker_event(
                                generation,
                                SearchWorkerEvent::Prepared(event),
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
    pub fn count_current_log_matches(&mut self, cx: &mut Context<Self>) {
        self.start_current_log_count_scan(cx);
    }

    /// 跳转到当前日志中的下一个关键字命中。
    pub fn activate_next_current_log_match(&mut self, cx: &mut Context<Self>) {
        if self.try_activate_cached_quick_match(QuickMatchAction::Next) {
            return;
        }
        self.start_current_log_navigation_scan(CurrentLogMatchDirection::Next, cx);
    }

    /// 跳转到当前日志中的上一个关键字命中。
    pub fn activate_previous_current_log_match(&mut self, cx: &mut Context<Self>) {
        if self.try_activate_cached_quick_match(QuickMatchAction::Previous) {
            return;
        }
        self.start_current_log_navigation_scan(CurrentLogMatchDirection::Previous, cx);
    }

    /// 清理当前日志快速查找状态，并取消尚未完成的扫描任务。
    pub fn clear_quick_log_search_state(&mut self) {
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
    pub fn cancel_log_search(&mut self) {
        if let Some(cancel_token) = self.log_search.cancel_token.take() {
            cancel_token.store(true, Ordering::Relaxed);
        }
        if self.log_search.task_state.is_running() {
            self.log_search.task_state = SearchTaskState::Cancelled;
            self.log_search.message = Some("搜索已取消".to_string());
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
    pub fn activate_search_result(&mut self, result_index: usize, cx: &mut Context<Self>) {
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
    pub fn should_show_log_search_results(&self) -> bool {
        !self.log_search.results.is_empty()
            || self.log_search.task_state.is_running()
            || self.log_search.message.is_some()
    }

    /// 关闭底部搜索结果面板并清理当前正文高亮。
    pub fn close_log_search_results_panel(&mut self) {
        self.clear_quick_log_search_state();
        self.log_search.results.clear();
        self.log_search.result_groups.clear();
        self.log_search.visible_result_items.clear();
        self.log_search.collapsed_result_groups.clear();
        self.log_search.result_list_content_width = 0.0;
        self.log_search.progress = SearchProgress::default();
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
        if let Some(cancel_token) = self.log_search.cancel_token.take() {
            cancel_token.store(true, Ordering::Relaxed);
        }
        self.log_search.progress = SearchProgress::default();
        self.log_search.task_state = SearchTaskState::Idle;
        self.log_search.results.clear();
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
    pub fn handle_source_tree_click(
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

        if !source.kind.is_log_candidate() {
            return;
        }

        if modifiers.shift {
            self.select_source_tree_range_for_search(source_id);
            self.placeholder_notice = format!(
                "已选择 {} 个搜索文件",
                self.selected_search_source_ids.len()
            );
            return;
        }

        if modifiers.secondary() {
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

        self.selected_search_source_ids.clear();
        self.selected_search_source_ids.insert(source_id);
        self.last_source_selection_anchor = Some(source_id);
        self.select_source(source_id);
        self.request_open_log_content(source_id, cx);
        self.scroll_source_into_view(source_id);
    }

    /// 返回来源节点是否属于搜索多选集合，用于来源树绘制选中态。
    pub fn is_source_selected_for_search(&self, source_id: SourceId) -> bool {
        self.selected_search_source_ids.contains(&source_id)
    }

    /// 返回输入框当前选区范围。
    pub fn log_search_input_selection_range(
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
    pub fn focus_log_search_input(&mut self, input_kind: LogSearchInputKind) {
        self.clear_log_search_input_focus();
        let input = self.log_search_input_mut(input_kind);
        input.is_focused = true;
        input.cursor = character_count(&input.value);
        input.selection_anchor = None;
        input.selection_drag = None;
    }

    /// 清空指定搜索输入框，并保持输入焦点。
    pub fn clear_log_search_input(&mut self, input_kind: LogSearchInputKind) {
        let input = self.log_search_input_mut(input_kind);
        input.value.clear();
        input.cursor = 0;
        input.selection_anchor = None;
        input.selection_drag = None;
        input.is_focused = true;
        if input_kind == LogSearchInputKind::Keyword {
            self.clear_quick_log_search_state();
        }
    }

    /// 处理搜索窗口输入框键盘输入。
    pub fn handle_log_search_input_key(
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
    pub fn begin_log_search_input_pointer_selection(
        &mut self,
        input_kind: LogSearchInputKind,
        character_index: usize,
        granularity: TextSelectionGranularity,
    ) {
        self.focus_log_search_input(input_kind);
        let anchor_range =
            self.log_search_input_range_for_granularity(input_kind, character_index, granularity);
        self.apply_log_search_input_pointer_range(
            input_kind,
            anchor_range.clone(),
            anchor_range.clone(),
        );
        self.log_search_input_mut(input_kind).selection_drag = Some(InputTextSelectionDrag {
            anchor_range,
            granularity,
        });
    }

    /// 鼠标拖拽过程中扩展搜索输入框选区。
    pub fn update_log_search_input_pointer_selection(
        &mut self,
        input_kind: LogSearchInputKind,
        character_index: usize,
    ) {
        let Some(drag) = self.log_search_input(input_kind).selection_drag.clone() else {
            return;
        };
        let focus_range = self.log_search_input_range_for_granularity(
            input_kind,
            character_index,
            drag.granularity,
        );
        self.apply_log_search_input_pointer_range(input_kind, drag.anchor_range, focus_range);
    }

    /// 结束搜索输入框鼠标选择。
    pub fn finish_log_search_input_pointer_selection(&mut self, input_kind: LogSearchInputKind) {
        let input = self.log_search_input_mut(input_kind);
        input.selection_drag = None;
        if input.selection_anchor == Some(input.cursor) {
            input.selection_anchor = None;
        }
    }

    /// 当前输入框是否正在鼠标拖拽选择。
    pub fn is_log_search_input_pointer_selecting(&self, input_kind: LogSearchInputKind) -> bool {
        self.log_search_input(input_kind).selection_drag.is_some()
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
                    Some(format!("已准备 {} 个搜索目标，开始搜索", event.total_files));
                self.placeholder_notice = self
                    .log_search
                    .message
                    .clone()
                    .unwrap_or_else(|| "已准备搜索目标".to_string());
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
    pub fn apply_search_progress(&mut self, generation: usize, progress: SearchProgress) {
        if self.log_search.generation != generation {
            return;
        }
        self.log_search.progress = progress;
    }

    /// 追加搜索结果批次；generation 不一致时丢弃。
    pub fn append_search_results(&mut self, generation: usize, mut results: Vec<SearchResult>) {
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
        self.log_search.results.append(&mut results);
    }

    /// 切换搜索结果文件分组展开状态。
    pub fn toggle_search_result_group(&mut self, group_index: usize) {
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

    /// 开始拖拽搜索结果面板高度。
    pub fn begin_search_result_panel_resize(&mut self, cursor_y: Pixels) {
        self.log_search.result_panel_resize_drag = Some(SearchResultPanelResizeDrag {
            start_y: cursor_y,
            start_height: self.log_search.result_panel_height,
        });
    }

    /// 拖拽更新搜索结果面板高度。
    pub fn resize_search_result_panel(&mut self, cursor_y: Pixels) -> bool {
        let Some(drag) = self.log_search.result_panel_resize_drag else {
            return false;
        };
        let delta = f32::from(drag.start_y - cursor_y);
        let next_height = (drag.start_height + delta).clamp(
            SEARCH_RESULT_PANEL_HEIGHT_MIN,
            SEARCH_RESULT_PANEL_HEIGHT_MAX,
        );
        if (next_height - self.log_search.result_panel_height).abs() < f32::EPSILON {
            return false;
        }

        self.log_search.result_panel_height = next_height;
        true
    }

    /// 结束搜索结果面板高度拖拽。
    pub fn finish_search_result_panel_resize(&mut self) -> bool {
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
    fn select_source_tree_range_for_search(&mut self, target_id: SourceId) {
        let Some(anchor_id) = self.last_source_selection_anchor else {
            self.selected_search_source_ids.clear();
            self.selected_search_source_ids.insert(target_id);
            self.last_source_selection_anchor = Some(target_id);
            return;
        };
        let visible_ids = self.visible_source_ids();
        let Some(anchor_index) = visible_ids.iter().position(|id| *id == anchor_id) else {
            // 原锚点已经不在当前可见树中时，用本次目标重建锚点，避免后续范围基于失效节点。
            self.selected_search_source_ids.clear();
            self.selected_search_source_ids.insert(target_id);
            self.last_source_selection_anchor = Some(target_id);
            return;
        };
        let Some(target_index) = visible_ids.iter().position(|id| *id == target_id) else {
            return;
        };

        let (start, end) = if anchor_index <= target_index {
            (anchor_index, target_index)
        } else {
            (target_index, anchor_index)
        };
        let selected = visible_ids[start..=end]
            .iter()
            .filter(|source_id| {
                self.source_registry
                    .node(**source_id)
                    .is_some_and(|node| node.kind.is_log_candidate())
            })
            .copied()
            .collect::<BTreeSet<_>>();
        if !selected.is_empty() {
            self.selected_search_source_ids = selected;
        }
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
                active_range: None,
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
        self.log_search.keyword_input.is_focused = false;
        self.log_search.keyword_input.selection_drag = None;
        self.log_search.directory_input.is_focused = false;
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
    fn log_search_input(&self, input_kind: LogSearchInputKind) -> &LogSearchInputState {
        match input_kind {
            LogSearchInputKind::Keyword => &self.log_search.keyword_input,
            LogSearchInputKind::Directory => &self.log_search.directory_input,
        }
    }

    /// 返回指定搜索输入框可变状态。
    fn log_search_input_mut(&mut self, input_kind: LogSearchInputKind) -> &mut LogSearchInputState {
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
        input.selection_drag = None;
        if input_kind == LogSearchInputKind::Keyword {
            self.clear_quick_log_search_state();
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
        let app_context: &gpui::App = (&*cx).borrow();
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
        let app_context: &gpui::App = (&*cx).borrow();
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

    /// 根据选择粒度返回搜索输入框目标范围。
    fn log_search_input_range_for_granularity(
        &self,
        input_kind: LogSearchInputKind,
        character_index: usize,
        granularity: TextSelectionGranularity,
    ) -> Range<usize> {
        let input = self.log_search_input(input_kind);
        let text_length = character_count(&input.value);
        let cursor = character_index.min(text_length);
        match granularity {
            TextSelectionGranularity::Character => cursor..cursor,
            TextSelectionGranularity::Word => {
                word_range_at(&input.value, cursor).unwrap_or(cursor..cursor)
            }
            TextSelectionGranularity::Line => 0..text_length,
        }
    }

    /// 合并输入框鼠标选择范围。
    fn apply_log_search_input_pointer_range(
        &mut self,
        input_kind: LogSearchInputKind,
        anchor_range: Range<usize>,
        focus_range: Range<usize>,
    ) {
        let input = self.log_search_input_mut(input_kind);
        if focus_range.end <= anchor_range.start {
            input.selection_anchor = Some(anchor_range.end);
            input.cursor = focus_range.start;
        } else {
            input.selection_anchor = Some(anchor_range.start);
            input.cursor = anchor_range.end.max(focus_range.end);
        }
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
    registry: SourceRegistry,
    loader_config: crate::config::LoaderConfig,
    mut request: SearchRequest,
    cancel_token: Arc<AtomicBool>,
    sender: mpsc::Sender<SearchWorkerEvent>,
) {
    std::thread::spawn(move || {
        let preparation = prepare_directory_search_targets(registry, directory_id, loader_config);
        let preparation = match preparation {
            Ok(preparation) => preparation,
            Err(message) => {
                let _ = sender.send(SearchWorkerEvent::Failed(message));
                return;
            }
        };

        if cancel_token.load(Ordering::Relaxed) {
            let _ = sender.send(SearchWorkerEvent::Finished(SearchTaskSummary {
                was_cancelled: true,
                ..SearchTaskSummary::default()
            }));
            return;
        }

        if preparation.targets.is_empty() {
            let message = if preparation.errors.is_empty() {
                "目录下没有日志文件".to_string()
            } else {
                format!("目录搜索目标加载失败：{}", preparation.errors.join("；"))
            };
            let _ = sender.send(SearchWorkerEvent::Failed(message));
            return;
        }

        request.targets = preparation.targets;
        let target_count = request.targets.len();
        let prepare_errors = preparation.errors;
        let _ = sender.send(SearchWorkerEvent::Prepared(SearchPreparedEvent {
            registry: Some(preparation.registry),
            total_files: target_count,
        }));

        let progress_sender = sender.clone();
        let result_sender = sender.clone();
        let mut summary = SearchEngine::search(
            request,
            move |progress| {
                let _ = progress_sender.send(SearchWorkerEvent::Progress(progress));
            },
            move |results| {
                let _ = result_sender.send(SearchWorkerEvent::Results(results));
            },
            cancel_token,
        );
        summary.errors.extend(prepare_errors);
        let _ = sender.send(SearchWorkerEvent::Finished(summary));
    });
}

/// 在后台补齐目录搜索所需的来源树，并收集可搜索日志目标。
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
fn collect_loaded_log_targets_under_registry(
    registry: &SourceRegistry,
    directory_id: SourceId,
) -> Vec<SearchTarget> {
    let mut targets = Vec::new();
    collect_loaded_log_targets_recursive(registry, directory_id, &mut targets);
    targets
}

/// 递归收集来源树快照中的日志候选。
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
fn reset_log_search_input_state(input: &mut LogSearchInputState) {
    input.value.clear();
    input.cursor = 0;
    input.selection_anchor = None;
    input.selection_drag = None;
    input.is_focused = false;
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::app::{ArgusTab, LogTextPosition, LogTextSelection};
    use crate::config::{ConfigManager, LoaderConfig};
    use crate::loader::{
        LogSourceLoader, SourceKind, SourceLocation, SourceMetadata, SourceRegistry, SourceTreeNode,
    };
    use crate::reader::log_file_reader::{LogFileReader, LogOpenState, OpenLogRequest};

    /// 测试配置路径计数器，避免搜索状态测试污染真实用户配置。
    static NEXT_TEST_CONFIG_ID: AtomicUsize = AtomicUsize::new(0);

    /// 构造隔离配置路径的应用状态。
    fn test_app() -> ArgusApp {
        let id = NEXT_TEST_CONFIG_ID.fetch_add(1, Ordering::Relaxed);
        let config_dir =
            std::env::temp_dir().join(format!("argus-log-search-test-{}-{id}", std::process::id()));
        let _ = std::fs::remove_dir_all(&config_dir);
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
        let root = std::env::temp_dir().join(format!(
            "argus-search-lazy-dir-{}-{}",
            std::process::id(),
            NEXT_TEST_CONFIG_ID.fetch_add(1, Ordering::Relaxed)
        ));
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
            match_ranges: vec![0..5],
        });

        app.reset_log_search_runtime_state();

        assert!(app.selected_search_source_ids.is_empty());
        assert!(app.log_search.results.is_empty());
        assert!(app.log_search.keyword_input.value.is_empty());
        assert!(app.log_search.directory_input.value.is_empty());
        assert_eq!(app.log_search.directory_source_id, None);
        assert_eq!(app.log_search.task_state, SearchTaskState::Idle);
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
        let log_path = std::env::temp_dir().join(format!(
            "argus-selected-keyword-{}-{}.log",
            std::process::id(),
            NEXT_TEST_CONFIG_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&log_path, "first ERROR line\nsecond line").unwrap();
        let handle = LogFileReader::open(OpenLogRequest {
            source_id: first_id,
            location: SourceLocation::LocalPath(log_path.clone()),
            label: "app.log".to_string(),
            default_encoding: "utf-8".to_string(),
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
        assert!(!app.is_case_sensitive);
        assert!(!app.is_regex_enabled);
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
            },
            SearchResult {
                source_id: first_id,
                label: "app.log".to_string(),
                path: "logs/app.log".to_string(),
                line_number: 2,
                line_text: "ERROR".to_string(),
                match_ranges: vec![0..5],
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
            match_ranges: vec![0..5],
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
                    match_ranges: vec![0..5],
                },
                SearchResult {
                    source_id: first_id,
                    label: "app.log".to_string(),
                    path: "logs/app.log".to_string(),
                    line_number: 2,
                    line_text: "ERROR two".to_string(),
                    match_ranges: vec![0..5],
                },
                SearchResult {
                    source_id: second_id,
                    label: "error.log".to_string(),
                    path: "logs/error.log".to_string(),
                    line_number: 3,
                    line_text: "ERROR three".to_string(),
                    match_ranges: vec![0..5],
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
    }

    /// 验证搜索结果面板拖拽高度会被限制在合理范围内。
    #[test]
    fn search_result_panel_resize_is_clamped() {
        let mut app = test_app();

        app.begin_search_result_panel_resize(gpui::px(300.0));
        assert!(app.resize_search_result_panel(gpui::px(-500.0)));
        assert_eq!(
            app.log_search.result_panel_height,
            SEARCH_RESULT_PANEL_HEIGHT_MAX
        );

        app.begin_search_result_panel_resize(gpui::px(300.0));
        assert!(app.resize_search_result_panel(gpui::px(900.0)));
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
}
