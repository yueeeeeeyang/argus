//! 文件职责：提取 Runtime 请求日志分析的标签创建、过滤、排序、SQL 弹窗和单元格选区等方法到独立子模块。

use super::*;

impl ArgusApp {
    pub fn open_runtime_analysis_tab(&mut self, source_id: SourceId, cx: &mut Context<Self>) {
        if !self.ensure_source_directory_ready_for_analysis(
            source_id,
            PendingSourceAnalysisAction::Runtime { source_id },
            cx,
        ) {
            return;
        }

        let targets = self.runtime_targets_for_context(source_id);
        if targets.is_empty() {
            self.placeholder_notice = "请选择至少一个 Runtime 日志文件或本地目录".to_string();
            return;
        }

        let background_targets = targets.clone();
        let Some((analysis_id, generation)) = self.create_runtime_analysis_tab_state(targets)
        else {
            self.placeholder_notice = "未找到可读取的 Runtime 日志来源".to_string();
            return;
        };

        let default_encoding = self.selected_encoding.clone();
        let loader_config = self.config.loader.clone();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    analyze_runtime_targets(background_targets, default_encoding, loader_config)
                })
                .await;

            view.update(cx, |app, cx| {
                app.apply_runtime_analysis_result(analysis_id, generation, result, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 返回指定 Runtime 分析状态。
    pub fn runtime_analysis_state(&self, analysis_id: usize) -> Option<&RuntimeAnalysisState> {
        self.runtime_analyses.get(&analysis_id)
    }

    /// 返回指定 Runtime 分析状态的可变引用。
    pub fn runtime_analysis_state_mut(
        &mut self,
        analysis_id: usize,
    ) -> Option<&mut RuntimeAnalysisState> {
        self.runtime_analyses.get_mut(&analysis_id)
    }

    /// 创建 Runtime 分析 tab 和加载状态；后台任务由调用方负责启动。
    pub(super) fn create_runtime_analysis_tab_state(
        &mut self,
        targets: Vec<RuntimeAnalysisTarget>,
    ) -> Option<(usize, usize)> {
        if targets.is_empty() {
            return None;
        }

        let analysis_id = self.next_runtime_analysis_id;
        self.next_runtime_analysis_id = self.next_runtime_analysis_id.saturating_add(1);
        let title = if targets.len() > 1 {
            format!("Runtime分析({})", targets.len())
        } else {
            "Runtime分析".to_string()
        };
        let tab_id = if self.tabs.len() == 1 && matches!(self.tabs[0].kind, TabKind::Empty) {
            self.tabs[0].id
        } else {
            let next_id = self.next_tab_id;
            self.next_tab_id += 1;
            self.tabs.push(ArgusTab {
                id: next_id,
                title: String::new(),
                kind: TabKind::Empty,
            });
            next_id
        };

        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) {
            tab.title = title.clone();
            tab.kind = TabKind::RuntimeAnalysis { analysis_id };
        }
        self.active_tab_id = tab_id;
        self.log_tab_view_states.remove(&tab_id);

        let generation = 1;
        self.runtime_analyses.insert(
            analysis_id,
            RuntimeAnalysisState {
                id: analysis_id,
                title: title.clone(),
                targets,
                generation,
                view: RuntimeAnalysisView::Summary,
                result_type: RuntimeAnalysisResultType::Statistics,
                summary_sort_key: RuntimeSummarySortKey::RequestCount,
                summary_sort_direction: RuntimeSortDirection::Descending,
                request_sort_key: RuntimeRequestSortKey::RequestTime,
                request_sort_direction: RuntimeSortDirection::Descending,
                sql_sort_key: RuntimeSqlSortKey::ExecuteDuration,
                sql_sort_direction: RuntimeSortDirection::Descending,
                filter_keyword_input: SettingsTextInputState::default(),
                filter_username_input: SettingsTextInputState::default(),
                filter_start_time_input: SettingsTextInputState::default(),
                filter_end_time_input: SettingsTextInputState::default(),
                applied_filter_keyword: String::new(),
                applied_filter_username: String::new(),
                applied_filter_start_time: String::new(),
                applied_filter_end_time: String::new(),
                filter_input_generation: 0,
                filter_task_generation: 0,
                is_filter_pending: false,
                is_filter_computing: false,
                open_time_picker: None,
                cell_selection: None,
                cell_selection_drag: None,
                hovered_sql_cell: None,
                sql_text_dialog: None,
                summary_scroll: UniformListScrollHandle::new(),
                request_scroll: UniformListScrollHandle::new(),
                sql_scroll: UniformListScrollHandle::new(),
                sql_frequency_scroll: UniformListScrollHandle::new(),
                sql_frequency_detail_scroll: UniformListScrollHandle::new(),
                slow_sql_scroll: UniformListScrollHandle::new(),
                sql_frequency_detail_sql: None,
                slow_sql_detail_sql: None,
                runtime_filter_rows_cache: None,
                sql_frequency_rows_task_generation: 0,
                slow_sql_rows_task_generation: 0,
                is_sql_frequency_rows_computing: false,
                is_slow_sql_rows_computing: false,
                sql_frequency_rows_computing_filter: None,
                slow_sql_rows_computing_filter: None,
                sql_frequency_rows_cache: RefCell::new(None),
                sql_frequency_detail_rows_cache: RefCell::new(None),
                slow_sql_rows_cache: RefCell::new(None),
                summary_rows_cache: RefCell::new(None),
                request_indices_cache: RefCell::new(None),
                sql_indices_cache: RefCell::new(None),
                scrollbar_drag: None,
                sql_dialog_scroll: ScrollHandle::new(),
                task_state: RuntimeAnalysisTaskState::Loading {
                    message: "正在分析 Runtime 日志文件".to_string(),
                },
            },
        );
        self.placeholder_notice = format!("已创建 {title} 页签");

        Some((analysis_id, generation))
    }

    /// 根据右键来源节点生成 Runtime 分析输入；文件多选命中时沿用多选，目录直接递归解析。
    pub(super) fn runtime_targets_for_context(&mut self, source_id: SourceId) -> Vec<RuntimeAnalysisTarget> {
        let Some(node) = self.source_registry.node(source_id) else {
            return Vec::new();
        };

        if self.source_is_local_directory(source_id) {
            return self.runtime_targets_from_source_ids(&[source_id]);
        }
        if self.source_is_archive_directory(source_id) {
            let source_ids = self.loaded_descendant_analysis_source_ids(source_id);
            return self.runtime_targets_from_source_ids(&source_ids);
        }

        if !node.kind.is_log_candidate()
            && !self.is_source_selectable_for_search_selection(source_id)
        {
            return Vec::new();
        }

        if !self.selected_search_source_ids.contains(&source_id) {
            self.selected_search_source_ids.clear();
            self.selected_search_source_ids.insert(source_id);
            self.last_source_selection_anchor = Some(source_id);
            return self.runtime_targets_from_source_ids(&[source_id]);
        }

        let selected_ids = self.selected_search_source_ids.clone();
        let mut ordered_ids = self
            .visible_source_ids()
            .iter()
            .filter(|visible_id| selected_ids.contains(visible_id))
            .filter(|visible_id| {
                self.source_registry
                    .node(**visible_id)
                    .is_some_and(|node| node.kind.is_log_candidate())
                    || self.is_source_selectable_for_search_selection(**visible_id)
            })
            .copied()
            .collect::<Vec<_>>();
        for selected_id in selected_ids {
            if !ordered_ids.contains(&selected_id)
                && (self
                    .source_registry
                    .node(selected_id)
                    .is_some_and(|node| node.kind.is_log_candidate())
                    || self.is_source_selectable_for_search_selection(selected_id))
            {
                ordered_ids.push(selected_id);
            }
        }

        self.runtime_targets_from_source_ids(&ordered_ids)
    }

    /// 将来源树节点转换为 Runtime 分析目标。
    pub(super) fn runtime_targets_from_source_ids(
        &self,
        source_ids: &[SourceId],
    ) -> Vec<RuntimeAnalysisTarget> {
        source_ids
            .iter()
            .filter_map(|source_id| {
                let node = self.source_registry.node(*source_id)?;
                let kind = if matches!(node.kind, SourceKind::Directory) {
                    if !matches!(node.location, SourceLocation::LocalPath(_)) {
                        return None;
                    }
                    RuntimeAnalysisTargetKind::Directory
                } else if node.kind.is_log_candidate()
                    || self.is_source_selectable_for_search_selection(*source_id)
                {
                    RuntimeAnalysisTargetKind::File
                } else {
                    return None;
                };

                Some(RuntimeAnalysisTarget {
                    source_id: *source_id,
                    location: node.location.clone(),
                    archive_probe_node: self.runtime_archive_probe_node(*source_id),
                    label: node.label.clone(),
                    path: node.location.display_path(),
                    kind,
                })
            })
            .collect()
    }

    /// 为 Runtime 分析生成待探测压缩包快照；已识别日志节点不需要额外探测。
    pub(super) fn runtime_archive_probe_node(&self, source_id: SourceId) -> Option<SourceTreeNode> {
        let node = self.source_registry.node(source_id)?;
        (!node.kind.is_log_candidate() && self.is_source_selectable_for_search_selection(source_id))
            .then(|| node.clone())
    }

    /// 应用后台 Runtime 分析结果，过期 generation 会被忽略。
    pub(super) fn apply_runtime_analysis_result(
        &mut self,
        analysis_id: usize,
        generation: usize,
        result: RuntimeAnalysisResult,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            return;
        };
        if state.generation != generation {
            return;
        }

        let file_count = result.total_files;
        let request_count = result.request_count();
        let sql_count = result.total_sql_records;
        let skipped_count = result.skipped_count();
        state.task_state = RuntimeAnalysisTaskState::Ready(Arc::new(result));
        let pending_generation = state
            .is_filter_pending
            .then_some(state.filter_input_generation);
        self.placeholder_notice = format!(
            "Runtime 分析完成：{file_count} 个文件，{request_count} 个请求，{sql_count} 条 SQL，跳过 {skipped_count} 个文件"
        );
        if let Some(input_generation) = pending_generation {
            self.schedule_runtime_filter_apply(analysis_id, input_generation, cx);
        } else {
            self.ensure_runtime_sql_analysis_rows_for_current_type(analysis_id, cx);
        }
    }

    /// 标记 Runtime 过滤输入发生变化，并通过防抖任务延后应用。
    pub(super) fn queue_runtime_filter_refresh(&mut self, analysis_id: usize, cx: &mut Context<Self>) {
        self.trigger_runtime_filter_refresh(analysis_id, Some(cx));
    }

    /// 标记 Runtime 过滤输入变化；有 UI 上下文时同时安排防抖任务。
    pub(super) fn trigger_runtime_filter_refresh(
        &mut self,
        analysis_id: usize,
        cx: Option<&mut Context<Self>>,
    ) {
        if let Some(input_generation) = self.after_runtime_filter_changed(analysis_id)
            && let Some(cx) = cx
        {
            self.schedule_runtime_filter_apply(analysis_id, input_generation, cx);
        }
    }

    /// 启动 Runtime 过滤防抖任务；过期 generation 会在真正计算前被丢弃。
    pub(super) fn schedule_runtime_filter_apply(
        &mut self,
        analysis_id: usize,
        input_generation: usize,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |view, cx| {
            Timer::after(Duration::from_millis(RUNTIME_FILTER_DEBOUNCE_MS)).await;
            view.update(cx, |app, cx| {
                app.start_runtime_filter_apply_if_current(analysis_id, input_generation, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 在防抖完成后启动真正的 Runtime 过滤后台计算。
    pub(super) fn start_runtime_filter_apply_if_current(
        &mut self,
        analysis_id: usize,
        input_generation: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            return;
        };
        if state.filter_input_generation != input_generation {
            return;
        }

        let filter = runtime_filter_input_snapshot_from_state(state);
        let criteria = parse_runtime_analysis_filter_criteria(&filter);
        let current_filter = runtime_filter_applied_snapshot_from_state(state);
        if current_filter == filter && !state.is_filter_pending {
            return;
        }

        state.filter_task_generation = state.filter_task_generation.saturating_add(1);
        let task_generation = state.filter_task_generation;
        state.is_filter_pending = false;

        if !criteria.is_active() {
            apply_runtime_filter_snapshot_to_state(state, &filter);
            state.runtime_filter_rows_cache = None;
            state.is_filter_computing = false;
            reset_runtime_filter_result_view_state(state);
            self.placeholder_notice = "Runtime 过滤条件已更新".to_string();
            self.ensure_runtime_sql_analysis_rows_for_current_type(analysis_id, cx);
            return;
        }

        let RuntimeAnalysisTaskState::Ready(result) = &state.task_state else {
            state.is_filter_pending = true;
            state.is_filter_computing = false;
            return;
        };
        let result = result.clone();
        state.is_filter_computing = true;
        self.placeholder_notice = "正在后台应用 Runtime 过滤条件".to_string();

        cx.spawn(async move |view, cx| {
            let rows = cx
                .background_executor()
                .spawn(async move { build_runtime_analysis_filter_rows(result.as_ref(), filter) })
                .await;

            view.update(cx, |app, cx| {
                app.apply_runtime_filter_rows(
                    analysis_id,
                    input_generation,
                    task_generation,
                    rows,
                    cx,
                );
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 应用后台构建好的 Runtime 过滤行缓存。
    pub(super) fn apply_runtime_filter_rows(
        &mut self,
        analysis_id: usize,
        input_generation: usize,
        task_generation: usize,
        rows: RuntimeAnalysisFilterRows,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            return;
        };
        if state.filter_input_generation != input_generation
            || state.filter_task_generation != task_generation
        {
            return;
        }

        apply_runtime_filter_snapshot_to_state(state, &rows.filter);
        state.runtime_filter_rows_cache = Some(rows);
        state.is_filter_pending = false;
        state.is_filter_computing = false;
        reset_runtime_filter_result_view_state(state);
        self.placeholder_notice = "Runtime 过滤条件已更新".to_string();
        self.ensure_runtime_sql_analysis_rows_for_current_type(analysis_id, cx);
    }

    /// 当前页签是 SQL 分析类结果时，确保对应行数据已经进入后台懒计算。
    pub(super) fn ensure_runtime_sql_analysis_rows_for_current_type(
        &mut self,
        analysis_id: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(result_type) = self
            .runtime_analyses
            .get(&analysis_id)
            .map(|state| state.result_type)
        else {
            return;
        };

        match result_type {
            RuntimeAnalysisResultType::SqlFrequency => {
                self.ensure_runtime_sql_frequency_rows(analysis_id, cx)
            }
            RuntimeAnalysisResultType::SlowSql => {
                self.ensure_runtime_slow_sql_rows(analysis_id, cx)
            }
            RuntimeAnalysisResultType::Statistics => {}
        }
    }

    /// 启动 SQL 频率分析行数据的后台懒计算。
    pub(super) fn ensure_runtime_sql_frequency_rows(&mut self, analysis_id: usize, cx: &mut Context<Self>) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            return;
        };
        let filter = runtime_filter_applied_snapshot_from_state(state);
        if state
            .sql_frequency_rows_cache
            .borrow()
            .as_ref()
            .is_some_and(|cache| cache.filter == filter)
        {
            return;
        }
        if state.is_sql_frequency_rows_computing
            && state.sql_frequency_rows_computing_filter.as_ref() == Some(&filter)
        {
            return;
        }
        let RuntimeAnalysisTaskState::Ready(result) = &state.task_state else {
            return;
        };

        let result = result.clone();
        state.sql_frequency_rows_task_generation =
            state.sql_frequency_rows_task_generation.saturating_add(1);
        let task_generation = state.sql_frequency_rows_task_generation;
        state.is_sql_frequency_rows_computing = true;
        state.sql_frequency_rows_computing_filter = Some(filter.clone());
        state.sql_frequency_detail_rows_cache.borrow_mut().take();
        self.placeholder_notice = "正在计算 SQL 频率分析".to_string();

        cx.spawn(async move |view, cx| {
            let filter_for_task = filter.clone();
            let rows = cx
                .background_executor()
                .spawn(async move {
                    Arc::new(build_runtime_sql_frequency_rows_for_filter(
                        result.as_ref(),
                        &filter_for_task,
                    ))
                })
                .await;

            view.update(cx, |app, cx| {
                app.apply_runtime_sql_frequency_rows(analysis_id, task_generation, filter, rows);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 应用后台构建好的 SQL 频率分析行。
    pub(super) fn apply_runtime_sql_frequency_rows(
        &mut self,
        analysis_id: usize,
        task_generation: usize,
        filter: RuntimeSqlAnalysisFilterSnapshot,
        rows: Arc<Vec<RuntimeSqlFrequencyAnalysisRow>>,
    ) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            return;
        };
        if state.sql_frequency_rows_task_generation != task_generation
            || runtime_filter_applied_snapshot_from_state(state) != filter
        {
            return;
        }

        state.is_sql_frequency_rows_computing = false;
        state.sql_frequency_rows_computing_filter = None;
        state
            .sql_frequency_rows_cache
            .borrow_mut()
            .replace(RuntimeSqlFrequencyRowsCache { filter, rows });
        self.placeholder_notice = "SQL 频率分析计算完成".to_string();
    }

    /// 启动慢 SQL 分析行数据的后台懒计算。
    pub(super) fn ensure_runtime_slow_sql_rows(&mut self, analysis_id: usize, cx: &mut Context<Self>) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            return;
        };
        let filter = runtime_filter_applied_snapshot_from_state(state);
        if state
            .slow_sql_rows_cache
            .borrow()
            .as_ref()
            .is_some_and(|cache| cache.filter == filter)
        {
            return;
        }
        if state.is_slow_sql_rows_computing
            && state.slow_sql_rows_computing_filter.as_ref() == Some(&filter)
        {
            return;
        }
        let RuntimeAnalysisTaskState::Ready(result) = &state.task_state else {
            return;
        };

        let result = result.clone();
        state.slow_sql_rows_task_generation = state.slow_sql_rows_task_generation.saturating_add(1);
        let task_generation = state.slow_sql_rows_task_generation;
        state.is_slow_sql_rows_computing = true;
        state.slow_sql_rows_computing_filter = Some(filter.clone());
        self.placeholder_notice = "正在计算慢 SQL 分析".to_string();

        cx.spawn(async move |view, cx| {
            let filter_for_task = filter.clone();
            let rows = cx
                .background_executor()
                .spawn(async move {
                    Arc::new(build_runtime_slow_sql_rows_for_filter(
                        result.as_ref(),
                        &filter_for_task,
                    ))
                })
                .await;

            view.update(cx, |app, cx| {
                app.apply_runtime_slow_sql_rows(analysis_id, task_generation, filter, rows);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 应用后台构建好的慢 SQL 分析行。
    pub(super) fn apply_runtime_slow_sql_rows(
        &mut self,
        analysis_id: usize,
        task_generation: usize,
        filter: RuntimeSqlAnalysisFilterSnapshot,
        rows: Arc<Vec<RuntimeSlowSqlSummaryRow>>,
    ) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            return;
        };
        if state.slow_sql_rows_task_generation != task_generation
            || runtime_filter_applied_snapshot_from_state(state) != filter
        {
            return;
        }

        state.is_slow_sql_rows_computing = false;
        state.slow_sql_rows_computing_filter = None;
        state
            .slow_sql_rows_cache
            .borrow_mut()
            .replace(RuntimeSlowSqlRowsCache { filter, rows });
        self.placeholder_notice = "慢 SQL 分析计算完成".to_string();
    }

    /// 切换 Runtime 分析结果类型，并清理旧表格残留的交互状态。
    pub fn set_runtime_result_type(
        &mut self,
        analysis_id: usize,
        result_type: RuntimeAnalysisResultType,
        cx: Option<&mut Context<Self>>,
    ) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Runtime 分析结果".to_string();
            return;
        };
        if state.result_type == result_type {
            return;
        }

        state.result_type = result_type;
        state.cell_selection = None;
        state.cell_selection_drag = None;
        state.hovered_sql_cell = None;
        state.sql_text_dialog = None;
        state.scrollbar_drag = None;
        if result_type == RuntimeAnalysisResultType::Statistics {
            state.sql_frequency_detail_sql = None;
            state.slow_sql_detail_sql = None;
            // 切回统计分析只是页面切换，不代表 SQL 分析数据失效；
            // 频率/慢 SQL 列表缓存要保留，避免用户返回 SQL 页时重复后台计算。
            state.sql_frequency_detail_rows_cache.borrow_mut().take();
        }
        match result_type {
            RuntimeAnalysisResultType::Statistics => match state.view {
                RuntimeAnalysisView::Summary => {
                    state.summary_scroll = UniformListScrollHandle::new()
                }
                RuntimeAnalysisView::RequestDetails { .. } => {
                    state.request_scroll = UniformListScrollHandle::new()
                }
                RuntimeAnalysisView::SqlList { .. } => {
                    state.sql_scroll = UniformListScrollHandle::new()
                }
            },
            RuntimeAnalysisResultType::SqlFrequency => {
                state.sql_frequency_detail_sql = None;
                state.slow_sql_detail_sql = None;
                state.sql_frequency_scroll = UniformListScrollHandle::new();
                state.sql_frequency_detail_scroll = UniformListScrollHandle::new();
                state.sql_frequency_detail_rows_cache.borrow_mut().take();
            }
            RuntimeAnalysisResultType::SlowSql => {
                state.sql_frequency_detail_sql = None;
                state.slow_sql_detail_sql = None;
                state.slow_sql_scroll = UniformListScrollHandle::new()
            }
        }
        if let Some(cx) = cx {
            self.ensure_runtime_sql_analysis_rows_for_current_type(analysis_id, cx);
        }
    }

    /// 打开 SQL 频率分析中指定 SQL 结构的执行详情页。
    pub fn open_runtime_sql_frequency_detail(
        &mut self,
        analysis_id: usize,
        normalized_sql: String,
    ) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Runtime 分析结果".to_string();
            return;
        };
        state.result_type = RuntimeAnalysisResultType::SqlFrequency;
        state.sql_frequency_detail_sql = Some(normalized_sql);
        state.sql_frequency_detail_scroll = UniformListScrollHandle::new();
        state.cell_selection = None;
        state.cell_selection_drag = None;
        state.hovered_sql_cell = None;
        state.sql_text_dialog = None;
        state.scrollbar_drag = None;
        state.sql_frequency_detail_rows_cache.borrow_mut().take();
        self.placeholder_notice = "查看 SQL 频率详情".to_string();
    }

    /// 从 SQL 频率详情页返回 SQL 频率列表。
    pub fn show_runtime_sql_frequency_summary(&mut self, analysis_id: usize) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Runtime 分析结果".to_string();
            return;
        };
        state.result_type = RuntimeAnalysisResultType::SqlFrequency;
        state.sql_frequency_detail_sql = None;
        state.sql_frequency_scroll = UniformListScrollHandle::new();
        state.cell_selection = None;
        state.cell_selection_drag = None;
        state.hovered_sql_cell = None;
        state.sql_text_dialog = None;
        state.scrollbar_drag = None;
    }

    /// 打开慢 SQL 分析中指定 SQL 结构的执行详情页。
    pub fn open_runtime_slow_sql_detail(&mut self, analysis_id: usize, normalized_sql: String) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Runtime 分析结果".to_string();
            return;
        };
        state.result_type = RuntimeAnalysisResultType::SlowSql;
        state.slow_sql_detail_sql = Some(normalized_sql);
        state.slow_sql_scroll = UniformListScrollHandle::new();
        state.cell_selection = None;
        state.cell_selection_drag = None;
        state.hovered_sql_cell = None;
        state.sql_text_dialog = None;
        state.scrollbar_drag = None;
        state.sql_frequency_detail_rows_cache.borrow_mut().take();
        self.placeholder_notice = "查看慢 SQL 执行详情".to_string();
    }

    /// 从慢 SQL 详情页返回慢 SQL 聚合列表。
    pub fn show_runtime_slow_sql_summary(&mut self, analysis_id: usize) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Runtime 分析结果".to_string();
            return;
        };
        state.result_type = RuntimeAnalysisResultType::SlowSql;
        state.slow_sql_detail_sql = None;
        state.slow_sql_scroll = UniformListScrollHandle::new();
        state.cell_selection = None;
        state.cell_selection_drag = None;
        state.hovered_sql_cell = None;
        state.sql_text_dialog = None;
        state.scrollbar_drag = None;
    }

    /// 切换 Runtime 分析总览表排序字段。
    pub fn set_runtime_summary_sort(
        &mut self,
        analysis_id: usize,
        sort_key: RuntimeSummarySortKey,
    ) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Runtime 分析结果".to_string();
            return;
        };
        if state.summary_sort_key == sort_key {
            state.summary_sort_direction = state.summary_sort_direction.toggled();
        } else {
            state.summary_sort_key = sort_key;
            state.summary_sort_direction = default_runtime_summary_sort_direction(sort_key);
        }
        state.cell_selection = None;
        state.cell_selection_drag = None;
        state.hovered_sql_cell = None;
        state.sql_text_dialog = None;
        state.summary_scroll = UniformListScrollHandle::new();
    }

    /// 切换 Runtime 请求明细表排序字段。
    pub fn set_runtime_request_sort(
        &mut self,
        analysis_id: usize,
        sort_key: RuntimeRequestSortKey,
    ) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Runtime 分析结果".to_string();
            return;
        };
        if state.request_sort_key == sort_key {
            state.request_sort_direction = state.request_sort_direction.toggled();
        } else {
            state.request_sort_key = sort_key;
            state.request_sort_direction = default_runtime_request_sort_direction(sort_key);
        }
        state.cell_selection = None;
        state.cell_selection_drag = None;
        state.hovered_sql_cell = None;
        state.sql_text_dialog = None;
        state.request_scroll = UniformListScrollHandle::new();
    }

    /// 切换 Runtime SQL 明细表排序字段。
    pub fn set_runtime_sql_sort(&mut self, analysis_id: usize, sort_key: RuntimeSqlSortKey) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Runtime 分析结果".to_string();
            return;
        };
        if state.sql_sort_key == sort_key {
            state.sql_sort_direction = state.sql_sort_direction.toggled();
        } else {
            state.sql_sort_key = sort_key;
            state.sql_sort_direction = default_runtime_sql_sort_direction(sort_key);
        }
        state.cell_selection = None;
        state.cell_selection_drag = None;
        state.hovered_sql_cell = None;
        state.sql_scroll = UniformListScrollHandle::new();
    }

    /// 从 Runtime 总览进入指定请求地址的详情页。
    pub fn open_runtime_request_details(&mut self, analysis_id: usize, request_path: String) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Runtime 分析结果".to_string();
            return;
        };
        state.view = RuntimeAnalysisView::RequestDetails {
            request_path: request_path.clone(),
        };
        state.result_type = RuntimeAnalysisResultType::Statistics;
        state.request_scroll = UniformListScrollHandle::new();
        state.cell_selection = None;
        state.cell_selection_drag = None;
        state.hovered_sql_cell = None;
        state.sql_text_dialog = None;
        self.placeholder_notice = format!("查看 Runtime 请求详情：{request_path}");
    }

    /// 从 Runtime 请求详情进入指定请求日志的 SQL 列表。
    pub fn open_runtime_sql_list(
        &mut self,
        analysis_id: usize,
        request_path: String,
        request_index: usize,
    ) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Runtime 分析结果".to_string();
            return;
        };
        state.view = RuntimeAnalysisView::SqlList {
            request_path,
            request_index,
        };
        state.result_type = RuntimeAnalysisResultType::Statistics;
        state.sql_scroll = UniformListScrollHandle::new();
        state.cell_selection = None;
        state.cell_selection_drag = None;
        state.hovered_sql_cell = None;
        state.sql_text_dialog = None;
        self.placeholder_notice = "查看 Runtime SQL 列表".to_string();
    }

    /// 返回 Runtime 总览页。
    pub fn show_runtime_summary(&mut self, analysis_id: usize) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Runtime 分析结果".to_string();
            return;
        };
        state.view = RuntimeAnalysisView::Summary;
        state.result_type = RuntimeAnalysisResultType::Statistics;
        state.summary_scroll = UniformListScrollHandle::new();
        state.cell_selection = None;
        state.cell_selection_drag = None;
        state.hovered_sql_cell = None;
        state.sql_text_dialog = None;
    }

    /// 从 Runtime SQL 列表返回请求详情页。
    pub fn show_runtime_request_details(&mut self, analysis_id: usize, request_path: String) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Runtime 分析结果".to_string();
            return;
        };
        state.view = RuntimeAnalysisView::RequestDetails { request_path };
        state.result_type = RuntimeAnalysisResultType::Statistics;
        state.request_scroll = UniformListScrollHandle::new();
        state.cell_selection = None;
        state.cell_selection_drag = None;
        state.hovered_sql_cell = None;
        state.sql_text_dialog = None;
    }

    /// 更新 Runtime SQL 文本单元格悬浮状态。
    ///
    /// `cell_key` 标识当前悬浮的具体 SQL 记录或聚合行；返回值表示状态是否变化，需要触发界面刷新。
    pub fn set_runtime_sql_cell_hovered(
        &mut self,
        analysis_id: usize,
        cell_key: RuntimeSqlCellKey,
        is_hovered: bool,
    ) -> bool {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            return false;
        };
        if is_hovered {
            if state.hovered_sql_cell == Some(cell_key) {
                return false;
            }
            state.hovered_sql_cell = Some(cell_key);
            return true;
        }
        if state.hovered_sql_cell == Some(cell_key) {
            state.hovered_sql_cell = None;
            return true;
        }
        false
    }

    /// 打开 Runtime SQL 完整文本弹窗，弹窗内容保留 SQL 原始换行和缩进。
    pub fn open_runtime_sql_text_dialog(
        &mut self,
        analysis_id: usize,
        mut dialog: RuntimeSqlTextDialog,
    ) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Runtime 分析结果".to_string();
            return;
        };
        dialog.selection = None;
        dialog.selection_drag = None;
        state.sql_text_dialog = Some(dialog);
        state.cell_selection = None;
        state.cell_selection_drag = None;
        // 重新打开弹窗时将代码块滚动复位到顶部，并清理可能残留的弹窗滚动条拖拽。
        state.sql_dialog_scroll.set_offset(point(px(0.0), px(0.0)));
        state.scrollbar_drag = state
            .scrollbar_drag
            .filter(|drag| drag.table != RuntimeScrollbarTable::SqlDialog);
    }

    /// 关闭 Runtime SQL 完整文本弹窗。
    pub fn close_runtime_sql_text_dialog(&mut self, analysis_id: usize) -> bool {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            return false;
        };
        let closed = state.sql_text_dialog.take().is_some();
        if closed {
            // 弹窗关闭后清理可能残留的弹窗滚动条拖拽状态，避免影响表格滚动条。
            state.scrollbar_drag = state
                .scrollbar_drag
                .filter(|drag| drag.table != RuntimeScrollbarTable::SqlDialog);
        }
        closed
    }

    /// 清理 Runtime SQL 完整文本弹窗中的正文选区。
    pub fn clear_runtime_sql_text_selection(&mut self, analysis_id: usize) -> bool {
        let Some(dialog) = self
            .runtime_analyses
            .get_mut(&analysis_id)
            .and_then(|state| state.sql_text_dialog.as_mut())
        else {
            return false;
        };
        let had_selection = dialog.selection.take().is_some();
        let had_drag = dialog.selection_drag.take().is_some();
        had_selection || had_drag
    }

    /// 开始在 Runtime SQL 完整文本弹窗中选择文本。
    pub fn begin_runtime_sql_text_selection(
        &mut self,
        analysis_id: usize,
        line_index: usize,
        line: String,
        character_index: usize,
        granularity: TextSelectionGranularity,
    ) {
        let Some(dialog) = self
            .runtime_analyses
            .get_mut(&analysis_id)
            .and_then(|state| state.sql_text_dialog.as_mut())
        else {
            return;
        };

        let anchor_range =
            runtime_sql_text_range_for_granularity(line_index, &line, character_index, granularity);
        dialog.selection = Some(anchor_range.clone());
        dialog.selection_drag = Some(RuntimeSqlTextSelectionDrag {
            anchor_range,
            granularity,
        });
    }

    /// 拖拽更新 Runtime SQL 完整文本弹窗中的文本选区。
    pub fn update_runtime_sql_text_selection(
        &mut self,
        analysis_id: usize,
        line_index: usize,
        line: String,
        character_index: usize,
    ) -> bool {
        let Some(dialog) = self
            .runtime_analyses
            .get_mut(&analysis_id)
            .and_then(|state| state.sql_text_dialog.as_mut())
        else {
            return false;
        };
        let Some(drag) = dialog.selection_drag.clone() else {
            return false;
        };

        let focus_range = runtime_sql_text_range_for_granularity(
            line_index,
            &line,
            character_index,
            drag.granularity,
        );
        let anchor_start = drag.anchor_range.anchor;
        let anchor_end = drag.anchor_range.focus;
        let focus_start = focus_range.anchor;
        let focus_end = focus_range.focus;
        dialog.selection = Some(RuntimeSqlTextSelection {
            anchor: if runtime_sql_text_position_le(anchor_start, focus_start) {
                anchor_start
            } else {
                focus_start
            },
            focus: if runtime_sql_text_position_le(anchor_end, focus_end) {
                focus_end
            } else {
                anchor_end
            },
        });
        true
    }

    /// 结束 Runtime SQL 完整文本弹窗文本选择；空选区会被清理。
    pub fn finish_runtime_sql_text_selection(&mut self, analysis_id: usize) -> bool {
        let Some(dialog) = self
            .runtime_analyses
            .get_mut(&analysis_id)
            .and_then(|state| state.sql_text_dialog.as_mut())
        else {
            return false;
        };
        let had_drag = dialog.selection_drag.take().is_some();
        if dialog
            .selection
            .as_ref()
            .map_or(true, RuntimeSqlTextSelection::is_empty)
        {
            dialog.selection = None;
        }
        had_drag
    }

    /// 复制 Runtime SQL 完整文本弹窗中的当前选区。
    ///
    /// 返回值：是否存在可复制的 SQL 弹窗选区。
    pub fn copy_runtime_sql_text_selection(
        &mut self,
        analysis_id: usize,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(selected_text) = self
            .runtime_analyses
            .get(&analysis_id)
            .and_then(|state| state.sql_text_dialog.as_ref())
            .and_then(|dialog| {
                let selection = dialog.selection.as_ref()?;
                let lines = runtime_sql_text_lines(&dialog.sql_text);
                selected_runtime_sql_text_from_lines(&lines, selection)
            })
        else {
            return false;
        };

        let app_context: &gpui::App = (&*cx).borrow();
        app_context.write_to_clipboard(ClipboardItem::new_string(selected_text.clone()));
        self.placeholder_notice = format!("已复制 SQL 文本：{selected_text}");
        true
    }

    /// 开始在 Runtime 表格单元格中选择文本。
    ///
    /// 参数说明：
    /// - `analysis_id`：Runtime 分析页 ID。
    /// - `cell_key`：单元格稳定 key。
    /// - `text`：单元格完整文本。
    /// - `character_index`：鼠标按下位置命中的字符列。
    /// - `granularity`：按点击次数决定的选择粒度。
    pub fn begin_runtime_cell_selection(
        &mut self,
        analysis_id: usize,
        cell_key: String,
        text: String,
        character_index: usize,
        granularity: TextSelectionGranularity,
    ) {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Runtime 分析结果".to_string();
            return;
        };

        let anchor_range = runtime_cell_range_for_granularity(&text, character_index, granularity);
        state.cell_selection = Some(RuntimeTableCellSelection {
            cell_key: cell_key.clone(),
            text: text.clone(),
            anchor: anchor_range.start,
            focus: anchor_range.end,
        });
        state.cell_selection_drag = Some(RuntimeTableCellSelectionDrag {
            cell_key,
            text,
            anchor_range,
            granularity,
        });
    }

    /// 拖拽更新 Runtime 表格单元格中的文本选区。
    ///
    /// 返回值：本次拖拽是否命中当前分析页和当前单元格。
    pub fn update_runtime_cell_selection(
        &mut self,
        analysis_id: usize,
        cell_key: &str,
        character_index: usize,
    ) -> bool {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            return false;
        };
        let Some(drag) = state.cell_selection_drag.clone() else {
            return false;
        };
        if drag.cell_key != cell_key {
            return false;
        }

        let focus_range =
            runtime_cell_range_for_granularity(&drag.text, character_index, drag.granularity);
        state.cell_selection = Some(RuntimeTableCellSelection {
            cell_key: drag.cell_key,
            text: drag.text,
            anchor: drag.anchor_range.start.min(focus_range.start),
            focus: drag.anchor_range.end.max(focus_range.end),
        });
        true
    }

    /// 结束 Runtime 单元格文本选择；如果没有选中字符则清理空选区。
    ///
    /// 返回值：当前分析页是否存在需要结束的拖拽状态。
    pub fn finish_runtime_cell_selection(&mut self, analysis_id: usize) -> bool {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            return false;
        };
        let had_drag = state.cell_selection_drag.take().is_some();
        if state
            .cell_selection
            .as_ref()
            .and_then(RuntimeTableCellSelection::normalized_range)
            .is_none()
        {
            state.cell_selection = None;
        }
        had_drag
    }

    /// 复制当前 Runtime 分析页表格单元格中拖选的文本。
    pub fn copy_selected_runtime_cell(&mut self, analysis_id: usize, cx: &mut Context<Self>) {
        let Some((cell_text, range)) = self
            .runtime_analyses
            .get(&analysis_id)
            .and_then(|state| state.cell_selection.as_ref())
            .and_then(|selection| {
                selection
                    .normalized_range()
                    .map(|range| (selection.text.clone(), range))
            })
        else {
            self.placeholder_notice = "请先拖选一个 Runtime 表格单元格内容".to_string();
            return;
        };

        let selected_text = slice_character_range(&cell_text, range);
        let app_context: &gpui::App = (&*cx).borrow();
        app_context.write_to_clipboard(ClipboardItem::new_string(selected_text.clone()));
        self.placeholder_notice = format!("已复制 Runtime 单元格内容：{selected_text}");
    }

    /// 清理全部 Runtime 分析页的表格单元格文本选区。
    ///
    /// 返回值：是否确实清理了已有选区或拖拽状态。
    pub fn clear_runtime_cell_selection(&mut self) -> bool {
        let mut changed = false;
        for state in self.runtime_analyses.values_mut() {
            if state.cell_selection.take().is_some() {
                changed = true;
            }
            if state.cell_selection_drag.take().is_some() {
                changed = true;
            }
        }
        changed
    }

    /// 打开 Runtime 时间选择器，并关闭其它 Runtime 页签中已展开的时间面板。
    pub fn open_runtime_time_picker(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
    ) {
        if !matches!(
            input_kind,
            RuntimeFilterInputKind::StartTime | RuntimeFilterInputKind::EndTime
        ) {
            return;
        }
        for state in self.runtime_analyses.values_mut() {
            state.open_time_picker = None;
        }
        if let Some(state) = self.runtime_analyses.get_mut(&analysis_id) {
            state.open_time_picker = Some(input_kind);
        }
    }

    /// 关闭指定 Runtime 分析页的时间选择器。
    ///
    /// 返回值：是否关闭了一个已打开的面板。
    pub fn close_runtime_time_picker(&mut self, analysis_id: usize) -> bool {
        self.runtime_analyses
            .get_mut(&analysis_id)
            .and_then(|state| state.open_time_picker.take())
            .is_some()
    }

    /// 使用快捷动作设置 Runtime 时间过滤输入框。
    pub fn apply_runtime_time_picker_quick_action(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        action: RuntimeDateTimeQuickAction,
        cx: Option<&mut Context<Self>>,
    ) {
        let is_end = input_kind == RuntimeFilterInputKind::EndTime;
        if action == RuntimeDateTimeQuickAction::Clear {
            self.clear_runtime_filter_input(analysis_id, input_kind, cx);
            return;
        }

        let now = Local::now();
        let datetime = match action {
            RuntimeDateTimeQuickAction::TodayStart => now
                .date_naive()
                .and_hms_opt(0, 0, 0)
                .and_then(|datetime| Local.from_local_datetime(&datetime).single())
                .unwrap_or(now),
            RuntimeDateTimeQuickAction::Now => now,
            RuntimeDateTimeQuickAction::TodayEnd => now
                .date_naive()
                .and_hms_opt(23, 59, 59)
                .and_then(|datetime| Local.from_local_datetime(&datetime).single())
                .unwrap_or(now),
            RuntimeDateTimeQuickAction::Clear => now,
        };
        self.set_runtime_filter_time_value(analysis_id, input_kind, datetime, is_end, cx);
    }

    /// 按日期时间组件的步进按钮调整 Runtime 时间过滤输入框。
    pub fn adjust_runtime_filter_time(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        part: RuntimeDateTimePart,
        delta: i32,
        cx: Option<&mut Context<Self>>,
    ) {
        let is_end = input_kind == RuntimeFilterInputKind::EndTime;
        let current = self
            .runtime_filter_input(analysis_id, input_kind)
            .and_then(|input| parse_runtime_filter_datetime_value(&input.value, is_end))
            .unwrap_or_else(|| default_runtime_filter_datetime(is_end));
        let adjusted = adjust_runtime_datetime_part(current, part, delta);
        self.set_runtime_filter_time_value(analysis_id, input_kind, adjusted, is_end, cx);
    }

    /// 设置 Runtime 时间过滤输入框的日期部分，并保留当前时分秒。
    ///
    /// 参数说明：
    /// - `analysis_id`：Runtime 分析页 ID。
    /// - `input_kind`：开始时间或结束时间输入框。
    /// - `year`、`month`、`day`：日历面板中选中的本地日期。
    ///
    /// 说明：常见 Web 日期时间选择器在点选日期后仍保留时间选择能力；
    /// 因此这里只更新日期，不关闭浮层，方便用户继续微调时分秒。
    pub fn set_runtime_filter_date(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        year: i32,
        month: u32,
        day: u32,
        cx: Option<&mut Context<Self>>,
    ) {
        if !matches!(
            input_kind,
            RuntimeFilterInputKind::StartTime | RuntimeFilterInputKind::EndTime
        ) {
            return;
        }
        let is_end = input_kind == RuntimeFilterInputKind::EndTime;
        let current = self
            .runtime_filter_input(analysis_id, input_kind)
            .and_then(|input| parse_runtime_filter_datetime_value(&input.value, is_end))
            .unwrap_or_else(|| default_runtime_filter_datetime(is_end));
        let Some(date) = NaiveDate::from_ymd_opt(year, month, day) else {
            return;
        };
        let Some(naive) = date.and_hms_opt(current.hour(), current.minute(), current.second())
        else {
            return;
        };
        let datetime = Local
            .from_local_datetime(&naive)
            .single()
            .unwrap_or(current);
        self.set_runtime_filter_time_value(analysis_id, input_kind, datetime, is_end, cx);
    }

    /// 写入 Runtime 时间过滤输入框，并触发过滤结果刷新。
    pub(super) fn set_runtime_filter_time_value(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        datetime: chrono::DateTime<Local>,
        is_end: bool,
        cx: Option<&mut Context<Self>>,
    ) {
        let Some(input) = self.runtime_filter_input_mut(analysis_id, input_kind) else {
            return;
        };
        input.value = format_runtime_filter_datetime_value(datetime);
        input.cursor = character_count(&input.value);
        input.selection_anchor = None;
        input.marked_range = None;
        input.selection_drag = None;
        input.is_focused = true;
        if let Some(state) = self.runtime_analyses.get_mut(&analysis_id) {
            state.open_time_picker = Some(if is_end {
                RuntimeFilterInputKind::EndTime
            } else {
                RuntimeFilterInputKind::StartTime
            });
        }
        self.trigger_runtime_filter_refresh(analysis_id, cx);
    }

    /// 聚焦 Runtime 过滤输入框，并清理其它 Runtime 过滤输入框的临时输入法状态。
    pub fn focus_runtime_filter_input(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
    ) {
        for state in self.runtime_analyses.values_mut() {
            clear_runtime_filter_inputs_focus(state);
        }
        if let Some(input) = self.runtime_filter_input_mut(analysis_id, input_kind) {
            input.is_focused = true;
            input.marked_range = None;
        }
    }

    /// 清空 Runtime 过滤输入框，并立即刷新当前分析页过滤结果。
    pub fn clear_runtime_filter_input(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        cx: Option<&mut Context<Self>>,
    ) {
        let Some(input) = self.runtime_filter_input_mut(analysis_id, input_kind) else {
            return;
        };
        input.value.clear();
        input.cursor = 0;
        input.selection_anchor = None;
        input.marked_range = None;
        input.selection_drag = None;
        input.is_focused = true;
        self.trigger_runtime_filter_refresh(analysis_id, cx);
    }

    /// 处理 Runtime 过滤输入框键盘事件。
    pub fn handle_runtime_filter_input_key(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        keystroke: &Keystroke,
        cx: &mut Context<Self>,
    ) {
        let key = keystroke.key.to_lowercase();
        if keystroke.modifiers.secondary() {
            match key.as_str() {
                "a" => self.select_all_runtime_filter_input(analysis_id, input_kind),
                "c" => self.copy_runtime_filter_input_selection(analysis_id, input_kind, cx),
                "x" => self.cut_runtime_filter_input_selection(analysis_id, input_kind, cx),
                "v" => self.paste_runtime_filter_input_clipboard(analysis_id, input_kind, cx),
                "left" | "arrowleft" => self.move_runtime_filter_input_cursor(
                    analysis_id,
                    input_kind,
                    0,
                    keystroke.modifiers.shift,
                ),
                "right" | "arrowright" => {
                    let end = self
                        .runtime_filter_input(analysis_id, input_kind)
                        .map(|input| character_count(&input.value))
                        .unwrap_or_default();
                    self.move_runtime_filter_input_cursor(
                        analysis_id,
                        input_kind,
                        end,
                        keystroke.modifiers.shift,
                    );
                }
                _ => {}
            }
            return;
        }

        match key.as_str() {
            "backspace" => self.delete_runtime_filter_input_backward(analysis_id, input_kind, cx),
            "delete" => self.delete_runtime_filter_input_forward(analysis_id, input_kind, cx),
            "left" | "arrowleft" => self.move_runtime_filter_input_left(
                analysis_id,
                input_kind,
                keystroke.modifiers.shift,
            ),
            "right" | "arrowright" => self.move_runtime_filter_input_right(
                analysis_id,
                input_kind,
                keystroke.modifiers.shift,
            ),
            "home" => self.move_runtime_filter_input_cursor(
                analysis_id,
                input_kind,
                0,
                keystroke.modifiers.shift,
            ),
            "end" => {
                let end = self
                    .runtime_filter_input(analysis_id, input_kind)
                    .map(|input| character_count(&input.value))
                    .unwrap_or_default();
                self.move_runtime_filter_input_cursor(
                    analysis_id,
                    input_kind,
                    end,
                    keystroke.modifiers.shift,
                );
            }
            "escape" => {
                if let Some(input) = self.runtime_filter_input_mut(analysis_id, input_kind) {
                    input.is_focused = false;
                    input.marked_range = None;
                    input.selection_drag = None;
                }
            }
            _ => {
                if let Some(key_char) = keystroke.key_char.as_ref()
                    && !keystroke.modifiers.control
                    && !keystroke.modifiers.alt
                    && !keystroke.modifiers.platform
                    && !keystroke.modifiers.function
                    && !key_char.chars().any(char::is_control)
                {
                    self.insert_runtime_filter_input_text(analysis_id, input_kind, key_char, cx);
                }
            }
        }
    }

    /// 开始 Runtime 过滤输入框鼠标选择。
    pub fn begin_runtime_filter_input_pointer_selection(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        character_index: usize,
        granularity: TextSelectionGranularity,
    ) {
        self.focus_runtime_filter_input(analysis_id, input_kind);
        let Some(input) = self.runtime_filter_input_mut(analysis_id, input_kind) else {
            return;
        };
        let range = runtime_filter_input_range_for_granularity(input, character_index, granularity);
        input.cursor = range.end;
        input.selection_anchor = Some(range.start);
        input.marked_range = None;
        input.selection_drag = Some(InputTextSelectionDrag {
            anchor_range: range,
            granularity,
        });
    }

    /// 更新 Runtime 过滤输入框鼠标拖拽选区。
    pub fn update_runtime_filter_input_pointer_selection(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        character_index: usize,
    ) {
        let Some(input) = self.runtime_filter_input_mut(analysis_id, input_kind) else {
            return;
        };
        let Some(drag) = input.selection_drag.clone() else {
            return;
        };
        let focus_range =
            runtime_filter_input_range_for_granularity(input, character_index, drag.granularity);
        input.selection_anchor = Some(drag.anchor_range.start.min(focus_range.start));
        input.marked_range = None;
        input.cursor = drag.anchor_range.end.max(focus_range.end);
    }

    /// 结束 Runtime 过滤输入框鼠标选择。
    pub fn finish_runtime_filter_input_pointer_selection(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
    ) {
        if let Some(input) = self.runtime_filter_input_mut(analysis_id, input_kind) {
            input.selection_drag = None;
        }
    }

    /// 返回指定 Runtime 过滤输入框的只读状态。
    pub fn runtime_filter_input(
        &self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
    ) -> Option<&SettingsTextInputState> {
        let state = self.runtime_analyses.get(&analysis_id)?;
        Some(match input_kind {
            RuntimeFilterInputKind::Keyword => &state.filter_keyword_input,
            RuntimeFilterInputKind::Username => &state.filter_username_input,
            RuntimeFilterInputKind::StartTime => &state.filter_start_time_input,
            RuntimeFilterInputKind::EndTime => &state.filter_end_time_input,
        })
    }

    /// 返回指定 Runtime 过滤输入框的可变状态。
    pub fn runtime_filter_input_mut(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
    ) -> Option<&mut SettingsTextInputState> {
        let state = self.runtime_analyses.get_mut(&analysis_id)?;
        Some(match input_kind {
            RuntimeFilterInputKind::Keyword => &mut state.filter_keyword_input,
            RuntimeFilterInputKind::Username => &mut state.filter_username_input,
            RuntimeFilterInputKind::StartTime => &mut state.filter_start_time_input,
            RuntimeFilterInputKind::EndTime => &mut state.filter_end_time_input,
        })
    }

    /// Runtime 过滤条件变化后标记待应用，真正过滤由防抖后台任务完成。
    pub fn after_runtime_filter_changed(&mut self, analysis_id: usize) -> Option<usize> {
        let Some(state) = self.runtime_analyses.get_mut(&analysis_id) else {
            return None;
        };
        state.filter_input_generation = state.filter_input_generation.saturating_add(1);
        state.is_filter_pending = true;
        state.cell_selection = None;
        state.cell_selection_drag = None;
        state.hovered_sql_cell = None;
        state.sql_text_dialog = None;
        state.sql_frequency_detail_rows_cache.borrow_mut().take();
        state.scrollbar_drag = None;
        self.placeholder_notice = "Runtime 过滤条件待应用".to_string();
        Some(state.filter_input_generation)
    }

    /// 全选 Runtime 过滤输入框内容。
    pub(super) fn select_all_runtime_filter_input(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
    ) {
        if let Some(input) = self.runtime_filter_input_mut(analysis_id, input_kind) {
            input.selection_anchor = Some(0);
            input.cursor = character_count(&input.value);
            input.marked_range = None;
            input.selection_drag = None;
        }
    }

    /// 复制 Runtime 过滤输入框当前选区。
    pub(super) fn copy_runtime_filter_input_selection(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        cx: &mut Context<Self>,
    ) {
        let Some(selected_text) = self.selected_runtime_filter_input_text(analysis_id, input_kind)
        else {
            return;
        };
        let app_context: &gpui::App = (&*cx).borrow();
        app_context.write_to_clipboard(ClipboardItem::new_string(selected_text));
    }

    /// 剪切 Runtime 过滤输入框当前选区。
    pub(super) fn cut_runtime_filter_input_selection(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        cx: &mut Context<Self>,
    ) {
        self.copy_runtime_filter_input_selection(analysis_id, input_kind, cx);
        if self.delete_runtime_filter_input_selection(analysis_id, input_kind) {
            self.queue_runtime_filter_refresh(analysis_id, cx);
        }
    }

    /// 粘贴剪贴板内容到 Runtime 过滤输入框。
    pub(super) fn paste_runtime_filter_input_clipboard(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        cx: &mut Context<Self>,
    ) {
        let app_context: &gpui::App = (&*cx).borrow();
        let Some(item) = app_context.read_from_clipboard() else {
            return;
        };
        if let Some(text) = item.text() {
            self.insert_runtime_filter_input_text(
                analysis_id,
                input_kind,
                &text.replace(['\n', '\r'], " "),
                cx,
            );
        }
    }

    /// 返回 Runtime 过滤输入框当前选中文本。
    pub(super) fn selected_runtime_filter_input_text(
        &self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
    ) -> Option<String> {
        let input = self.runtime_filter_input(analysis_id, input_kind)?;
        let range = normalized_runtime_filter_input_selection_range(input)?;
        Some(slice_character_range(&input.value, range))
    }

    /// 删除 Runtime 过滤输入框当前选区。
    pub(super) fn delete_runtime_filter_input_selection(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
    ) -> bool {
        let Some(input) = self.runtime_filter_input_mut(analysis_id, input_kind) else {
            return false;
        };
        let Some(range) = normalized_runtime_filter_input_selection_range(input) else {
            return false;
        };
        input.value = replace_character_range(&input.value, range.clone(), "");
        input.cursor = range.start;
        input.selection_anchor = None;
        input.marked_range = None;
        input.selection_drag = None;
        true
    }

    /// 向 Runtime 过滤输入框插入文本。
    pub(super) fn insert_runtime_filter_input_text(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        if text.is_empty() {
            return;
        }
        let _ = self.delete_runtime_filter_input_selection(analysis_id, input_kind);
        let Some(input) = self.runtime_filter_input_mut(analysis_id, input_kind) else {
            return;
        };
        let cursor = input.cursor.min(character_count(&input.value));
        input.value = replace_character_range(&input.value, cursor..cursor, text);
        input.cursor = cursor + character_count(text);
        input.selection_anchor = None;
        input.marked_range = None;
        input.selection_drag = None;
        self.queue_runtime_filter_refresh(analysis_id, cx);
    }

    /// 删除 Runtime 过滤输入框光标前一个字符。
    pub(super) fn delete_runtime_filter_input_backward(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        cx: &mut Context<Self>,
    ) {
        if self.delete_runtime_filter_input_selection(analysis_id, input_kind) {
            self.queue_runtime_filter_refresh(analysis_id, cx);
            return;
        }
        let Some(input) = self.runtime_filter_input_mut(analysis_id, input_kind) else {
            return;
        };
        if input.cursor == 0 {
            return;
        }
        let cursor = input.cursor.min(character_count(&input.value));
        input.value = replace_character_range(&input.value, cursor - 1..cursor, "");
        input.cursor = cursor - 1;
        input.marked_range = None;
        input.selection_drag = None;
        self.queue_runtime_filter_refresh(analysis_id, cx);
    }

    /// 删除 Runtime 过滤输入框光标后一个字符。
    pub(super) fn delete_runtime_filter_input_forward(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        cx: &mut Context<Self>,
    ) {
        if self.delete_runtime_filter_input_selection(analysis_id, input_kind) {
            self.queue_runtime_filter_refresh(analysis_id, cx);
            return;
        }
        let Some(input) = self.runtime_filter_input_mut(analysis_id, input_kind) else {
            return;
        };
        let text_length = character_count(&input.value);
        let cursor = input.cursor.min(text_length);
        if cursor >= text_length {
            return;
        }
        input.value = replace_character_range(&input.value, cursor..cursor + 1, "");
        input.cursor = cursor;
        input.marked_range = None;
        input.selection_drag = None;
        self.queue_runtime_filter_refresh(analysis_id, cx);
    }

    /// Runtime 过滤输入框光标左移。
    pub(super) fn move_runtime_filter_input_left(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        extend_selection: bool,
    ) {
        let cursor = self
            .runtime_filter_input(analysis_id, input_kind)
            .map(|input| input.cursor.saturating_sub(1))
            .unwrap_or_default();
        self.move_runtime_filter_input_cursor(analysis_id, input_kind, cursor, extend_selection);
    }

    /// Runtime 过滤输入框光标右移。
    pub(super) fn move_runtime_filter_input_right(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        extend_selection: bool,
    ) {
        let cursor = self
            .runtime_filter_input(analysis_id, input_kind)
            .map(|input| (input.cursor + 1).min(character_count(&input.value)))
            .unwrap_or_default();
        self.move_runtime_filter_input_cursor(analysis_id, input_kind, cursor, extend_selection);
    }

    /// 移动 Runtime 过滤输入框光标，并按需扩展选区。
    pub(super) fn move_runtime_filter_input_cursor(
        &mut self,
        analysis_id: usize,
        input_kind: RuntimeFilterInputKind,
        cursor: usize,
        extend_selection: bool,
    ) {
        let Some(input) = self.runtime_filter_input_mut(analysis_id, input_kind) else {
            return;
        };
        let text_length = character_count(&input.value);
        let cursor = cursor.min(text_length);
        if extend_selection {
            input.selection_anchor.get_or_insert(input.cursor);
        } else {
            input.selection_anchor = None;
        }
        input.cursor = cursor;
        input.marked_range = None;
        input.selection_drag = None;
    }

}
