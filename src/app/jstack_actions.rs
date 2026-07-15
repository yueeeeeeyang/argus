//! 文件职责：提取 Jstack 线程日志分析的标签创建、结果应用、线程筛选和选区交互等方法到独立子模块。

use super::*;

impl ArgusApp {
    /// 创建 Jstack 分析标签页，并启动后台读取与聚合任务。
    pub(crate) fn open_jstack_analysis_tab(&mut self, source_id: SourceId, cx: &mut Context<Self>) {
        if !self.ensure_source_directory_ready_for_analysis(
            source_id,
            PendingSourceAnalysisAction::Jstack { source_id },
            cx,
        ) {
            return;
        }

        let target_ids = self.jstack_source_ids_for_context(source_id);
        if target_ids.is_empty() {
            self.placeholder_notice = "请选择至少一个可分析的 Jstack 日志文件".to_string();
            return;
        }

        let targets = self.jstack_targets_from_source_ids(&target_ids);
        if targets.is_empty() {
            self.placeholder_notice = "未找到可读取的 Jstack 日志来源".to_string();
            return;
        }

        let background_targets = targets.clone();
        let Some((analysis_id, generation)) = self.create_jstack_analysis_tab_state(targets) else {
            self.placeholder_notice = "未找到可读取的 Jstack 日志来源".to_string();
            return;
        };

        let default_encoding = self.selected_encoding.clone();
        let loader_config = self.config.loader.clone();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    analyze_jstack_targets(background_targets, default_encoding, loader_config)
                })
                .await;

            view.update(cx, |app, cx| {
                app.apply_jstack_analysis_result(analysis_id, generation, result);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 返回指定 Jstack 分析状态。
    pub(crate) fn jstack_analysis_state(&self, analysis_id: usize) -> Option<&JstackAnalysisState> {
        self.jstack_analyses.get(&analysis_id)
    }

    /// 返回当前设置页配置的 Jstack 线程过滤器。
    pub(crate) fn jstack_thread_filter(&self) -> JstackThreadFilter {
        JstackThreadFilter::from_raw(
            &self.config.log_display.jstack_thread_name_filters,
            &self.config.log_display.jstack_stack_segment_filters,
        )
    }

    /// 根据当前配置重建所有 Jstack 分析页的可见行缓存。
    pub(crate) fn rebuild_all_jstack_visible_row_caches(&mut self) {
        let thread_filter = self.jstack_thread_filter();
        for state in self.jstack_analyses.values_mut() {
            state.rebuild_visible_row_cache(&thread_filter);
        }
    }

    /// 根据频率矩阵格子定位线程详情，并延迟组装完整堆栈记录。
    ///
    /// 参数说明：
    /// - `analysis_id`：Jstack 分析页签 ID。
    /// - `row_index`：频率矩阵中的线程行索引。
    /// - `active_snapshot_index`：用户点击的快照列索引。
    /// - `active_occurrence_index`：同快照内重复线程名的出现序号。
    /// - `cx`：主应用上下文，用于继续打开详情窗口。
    pub(crate) fn open_jstack_thread_detail_for_cell(
        &mut self,
        analysis_id: usize,
        row_index: usize,
        active_snapshot_index: usize,
        active_occurrence_index: usize,
        cx: &mut Context<Self>,
    ) {
        let detail_result: std::result::Result<(JstackThreadDetail, String), String> = (|| {
            let Some(state) = self.jstack_analyses.get(&analysis_id) else {
                return Err("未找到 Jstack 分析结果".to_string());
            };
            let JstackAnalysisTaskState::Ready(result) = &state.task_state else {
                return Err("Jstack 分析尚未完成".to_string());
            };
            let Some(row) = result.rows.get(row_index) else {
                return Err("未找到当前线程行".to_string());
            };
            let thread_name = row.thread_name.clone();
            let selected_cell_key = jstack_cell_selection_key(row_index, active_snapshot_index);

            // 详情窗口用于对比同一线程在不同日志快照中的表现；同一快照内重复出现时只取
            // 一个代表堆栈，避免单个文件里的多段采样被误看成多个日志文件。
            let occurrences = jstack_detail_occurrences_for_visible_cells(
                row,
                &state.active_states,
                active_snapshot_index,
                active_occurrence_index,
            );
            if occurrences.is_empty() {
                return Err("当前线程没有可展示的堆栈详情".to_string());
            }

            Ok((
                JstackThreadDetail {
                    thread_name,
                    thread_id: row.thread_id.clone(),
                    occurrences,
                },
                selected_cell_key,
            ))
        })();

        match detail_result {
            Ok((detail, selected_cell_key)) => {
                if let Some(state) = self.jstack_analyses.get_mut(&analysis_id) {
                    state.selected_cell_key = Some(selected_cell_key);
                    state.thread_name_selection = None;
                    state.thread_name_selection_drag = None;
                }
                self.open_jstack_thread_detail_window(
                    detail,
                    active_snapshot_index,
                    active_occurrence_index,
                    cx,
                );
            }
            Err(message) => {
                self.placeholder_notice = message;
            }
        }
    }

    /// 打开 Jstack 线程详情独立窗口。
    ///
    /// 参数说明：
    /// - `detail`：当前线程跨快照的完整堆栈记录。
    /// - `active_snapshot_index`：用户点击的快照序号，用于定位详情初始页。
    /// - `active_occurrence_index`：同快照内线程出现序号，用于重复线程名时定位具体堆栈。
    /// - `cx`：主应用上下文，用于创建无系统标题栏窗口。
    pub(crate) fn open_jstack_thread_detail_window(
        &mut self,
        detail: JstackThreadDetail,
        active_snapshot_index: usize,
        active_occurrence_index: usize,
        cx: &mut Context<Self>,
    ) {
        if detail.occurrences.is_empty() {
            self.placeholder_notice = "当前线程没有可展示的堆栈详情".to_string();
            return;
        }

        let initial_theme = self.theme.clone();
        let bounds = Bounds::centered(
            None,
            size(
                px(JSTACK_THREAD_DETAIL_WINDOW_WIDTH),
                px(JSTACK_THREAD_DETAIL_WINDOW_HEIGHT),
            ),
            cx,
        );
        let window_options = WindowOptions {
            titlebar: Some(frameless_resizable_titlebar()),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(
                px(JSTACK_THREAD_DETAIL_WINDOW_MIN_WIDTH),
                px(JSTACK_THREAD_DETAIL_WINDOW_MIN_HEIGHT),
            )),
            ..Default::default()
        };

        let thread_name = detail.display_label();
        match cx.open_window(window_options, move |_, cx| {
            cx.new(|cx| {
                JstackThreadDetailWindow::new(
                    initial_theme,
                    detail,
                    active_snapshot_index,
                    active_occurrence_index,
                    cx,
                )
            })
        }) {
            Ok(_) => {
                self.placeholder_notice = format!("已打开线程详情：{thread_name}");
            }
            Err(error) => {
                self.placeholder_notice = format!("打开线程详情失败：{error}");
            }
        }
    }

    /// 打开远程文件预览独立窗口，展示 worker 读取回传的文件内容。
    ///
    /// 参数说明：
    /// - `file_name`：文件名，用于窗口标题。
    /// - `content`：预览内容，可能为文本、二进制提示或读取错误。
    /// - `cx`：主应用上下文，用于创建无系统标题栏窗口并同步主题。
    pub(crate) fn open_file_preview_window(
        &mut self,
        file_name: String,
        content: crate::remote::remote_file::FilePreviewContent,
        cx: &mut Context<Self>,
    ) {
        let initial_theme = self.theme.clone();
        let app_entity = cx.entity();
        let bounds = Bounds::centered(
            None,
            size(
                px(FILE_PREVIEW_WINDOW_WIDTH),
                px(FILE_PREVIEW_WINDOW_HEIGHT),
            ),
            cx,
        );
        let window_options = WindowOptions {
            titlebar: Some(frameless_resizable_titlebar()),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(
                px(FILE_PREVIEW_WINDOW_MIN_WIDTH),
                px(FILE_PREVIEW_WINDOW_MIN_HEIGHT),
            )),
            ..Default::default()
        };

        match cx.open_window(window_options, move |_, cx| {
            cx.new(|cx| FilePreviewWindow::new(app_entity, initial_theme, file_name, content, cx))
        }) {
            Ok(_) => {
                self.placeholder_notice = "已打开文件预览".to_string();
            }
            Err(error) => {
                self.placeholder_notice = format!("打开文件预览失败：{error}");
            }
        }
    }

    /// 打开或更新 Jstack 方块内部悬浮气泡。
    ///
    /// 参数说明：
    /// - `preview`：当前方块的稳定 key、位置和预览内容。
    pub(crate) fn show_jstack_cell_hover_preview(&mut self, preview: JstackCellHoverPreview) {
        self.jstack_cell_hover_preview = Some(preview);
    }

    /// 清理 Jstack 方块内部悬浮气泡。
    pub(crate) fn clear_jstack_cell_hover_preview(&mut self) {
        self.jstack_cell_hover_preview = None;
    }

    /// 创建 Jstack 分析 tab 和加载状态；后台任务由调用方负责启动。
    pub(super) fn create_jstack_analysis_tab_state(
        &mut self,
        targets: Vec<JstackAnalysisTarget>,
    ) -> Option<(usize, usize)> {
        if targets.is_empty() {
            return None;
        }

        let analysis_id = self.next_jstack_analysis_id;
        self.next_jstack_analysis_id = self.next_jstack_analysis_id.saturating_add(1);
        let title = if targets.len() > 1 {
            format!("Jstack分析({})", targets.len())
        } else {
            "Jstack分析".to_string()
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
            tab.kind = TabKind::JstackAnalysis { analysis_id };
        }
        self.active_tab_id = tab_id;
        self.close_log_search_results_for_active_analysis_tab();
        self.log_tab_view_states.remove(&tab_id);

        let generation = 1;
        self.jstack_analyses.insert(
            analysis_id,
            JstackAnalysisState {
                generation,
                active_states: BTreeSet::from([JstackThreadState::Runnable]),
                is_thread_filter_enabled: true,
                thread_name_selection: None,
                thread_name_selection_drag: None,
                selected_cell_key: None,
                visible_row_indices: Vec::new(),
                filtered_row_count: 0,
                row_scroll: UniformListScrollHandle::new(),
                task_state: JstackAnalysisTaskState::Loading {
                    message: "正在分析 Jstack 日志文件".to_string(),
                },
            },
        );
        self.placeholder_notice = format!("已创建 {title} 页签");

        Some((analysis_id, generation))
    }

    /// 切换 Jstack 分析结果中的线程状态筛选项。
    pub(crate) fn toggle_jstack_state_filter(
        &mut self,
        analysis_id: usize,
        thread_state: JstackThreadState,
    ) {
        let thread_filter = self.jstack_thread_filter();
        let Some(state) = self.jstack_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Jstack 分析结果".to_string();
            return;
        };

        if !state.active_states.insert(thread_state) {
            state.active_states.remove(&thread_state);
        }
        state.rebuild_visible_row_cache(&thread_filter);
        state.row_scroll = UniformListScrollHandle::new();
        self.placeholder_notice = if state.active_states.is_empty() {
            "已隐藏全部 Jstack 线程状态".to_string()
        } else {
            let labels = state
                .active_states
                .iter()
                .map(|state| state.label())
                .collect::<Vec<_>>()
                .join(", ");
            format!("Jstack 状态筛选：{labels}")
        };
    }

    /// 开始在 Jstack 分析矩阵左侧线程名列中选择文本。
    ///
    /// 参数说明：
    /// - `analysis_id`：分析页 ID。
    /// - `thread_identity`：内部稳定线程身份，包含线程名和线程 ID。
    /// - `thread_name`：当前行显示的线程名文本。
    /// - `character_index`：鼠标按下位置命中的字符列。
    /// - `granularity`：按点击次数决定的选择粒度。
    pub(crate) fn begin_jstack_thread_name_selection(
        &mut self,
        analysis_id: usize,
        thread_identity: String,
        thread_name: String,
        character_index: usize,
        granularity: TextSelectionGranularity,
    ) {
        let Some(state) = self.jstack_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Jstack 分析结果".to_string();
            return;
        };

        let anchor_range =
            jstack_thread_name_range_for_granularity(&thread_name, character_index, granularity);
        state.thread_name_selection = Some(JstackThreadNameSelection {
            thread_identity: thread_identity.clone(),
            thread_name: thread_name.clone(),
            anchor: anchor_range.start,
            focus: anchor_range.end,
        });
        state.thread_name_selection_drag = Some(JstackThreadNameSelectionDrag {
            thread_identity,
            thread_name,
            anchor_range,
            granularity,
        });
        state.selected_cell_key = None;
    }

    /// 拖拽更新 Jstack 分析矩阵左侧线程名列中的文本选区。
    ///
    /// 返回值：本次拖拽是否命中当前分析页和当前线程行。
    pub(crate) fn update_jstack_thread_name_selection(
        &mut self,
        analysis_id: usize,
        thread_identity: &str,
        character_index: usize,
    ) -> bool {
        let Some(state) = self.jstack_analyses.get_mut(&analysis_id) else {
            return false;
        };
        let Some(drag) = state.thread_name_selection_drag.clone() else {
            return false;
        };
        if drag.thread_identity != thread_identity {
            return false;
        }

        let focus_range = jstack_thread_name_range_for_granularity(
            &drag.thread_name,
            character_index,
            drag.granularity,
        );
        state.thread_name_selection = Some(JstackThreadNameSelection {
            thread_identity: drag.thread_identity,
            thread_name: drag.thread_name,
            anchor: drag.anchor_range.start.min(focus_range.start),
            focus: drag.anchor_range.end.max(focus_range.end),
        });
        true
    }

    /// 结束 Jstack 线程名文本选择；如果没有选中字符则清理空选区。
    ///
    /// 返回值：当前分析页是否存在需要结束的拖拽状态。
    pub(crate) fn finish_jstack_thread_name_selection(&mut self, analysis_id: usize) -> bool {
        let Some(state) = self.jstack_analyses.get_mut(&analysis_id) else {
            return false;
        };
        let had_drag = state.thread_name_selection_drag.take().is_some();
        if state
            .thread_name_selection
            .as_ref()
            .and_then(JstackThreadNameSelection::normalized_range)
            .is_none()
        {
            state.thread_name_selection = None;
        }
        had_drag
    }

    /// 复制当前 Jstack 分析页左侧线程名列中拖选的文本。
    pub(crate) fn copy_selected_jstack_thread_name(
        &mut self,
        analysis_id: usize,
        cx: &mut Context<Self>,
    ) {
        let Some((thread_name, range)) = self
            .jstack_analyses
            .get(&analysis_id)
            .and_then(|state| state.thread_name_selection.as_ref())
            .and_then(|selection| {
                selection
                    .normalized_range()
                    .map(|range| (selection.thread_name.clone(), range))
            })
        else {
            self.placeholder_notice = "请先拖选一个 Jstack 线程名片段".to_string();
            return;
        };

        let selected_text = slice_character_range(&thread_name, range);
        let app_context: &gpui::App = (*cx).borrow();
        app_context.write_to_clipboard(ClipboardItem::new_string(selected_text.clone()));
        self.placeholder_notice = format!("已复制线程名片段：{selected_text}");
    }

    /// 切换 Jstack 分析页是否应用设置页中的线程堆栈过滤规则。
    pub(crate) fn toggle_jstack_thread_filter(&mut self, analysis_id: usize) {
        let thread_filter = self.jstack_thread_filter();
        let Some(state) = self.jstack_analyses.get_mut(&analysis_id) else {
            self.placeholder_notice = "未找到 Jstack 分析结果".to_string();
            return;
        };

        state.is_thread_filter_enabled = !state.is_thread_filter_enabled;
        state.rebuild_visible_row_cache(&thread_filter);
        state.row_scroll = UniformListScrollHandle::new();
        self.placeholder_notice = if state.is_thread_filter_enabled {
            "已启用 Jstack 配置过滤".to_string()
        } else {
            "已关闭 Jstack 配置过滤".to_string()
        };
    }

    /// 根据右键来源节点生成分析输入；多选命中时沿用多选，否则切换到单文件。
    pub(super) fn jstack_source_ids_for_context(&mut self, source_id: SourceId) -> Vec<SourceId> {
        if self.source_registry.node(source_id).is_none() {
            return Vec::new();
        }
        if self.source_is_local_directory(source_id) {
            return vec![source_id];
        }
        if self.source_is_archive_directory(source_id) {
            return self.loaded_descendant_analysis_source_ids(source_id);
        }
        if !self.source_supports_jstack_analysis(source_id) {
            return Vec::new();
        }

        if !self.selected_search_source_ids.contains(&source_id) {
            self.selected_search_source_ids.clear();
            self.selected_search_source_ids.insert(source_id);
            self.last_source_selection_anchor = Some(source_id);
            return vec![source_id];
        }

        let selected_ids = self.selected_search_source_ids.clone();
        let mut ordered_ids = self
            .visible_source_ids()
            .iter()
            .filter(|visible_id| selected_ids.contains(visible_id))
            .filter(|visible_id| self.is_source_selectable_for_search_selection(**visible_id))
            .copied()
            .collect::<Vec<_>>();
        for selected_id in selected_ids {
            if !ordered_ids.contains(&selected_id)
                && self.is_source_selectable_for_search_selection(selected_id)
            {
                ordered_ids.push(selected_id);
            }
        }

        ordered_ids
    }

    /// 收集已加载目录下所有可分析文件来源，保持来源树展示顺序。
    pub(super) fn loaded_descendant_analysis_source_ids(
        &self,
        parent_id: SourceId,
    ) -> Vec<SourceId> {
        let mut source_ids = Vec::new();
        self.collect_loaded_descendant_analysis_source_ids(parent_id, &mut source_ids);
        source_ids
    }

    /// 递归收集已加载后代文件；未加载目录不主动展开，避免在纯读取阶段阻塞 UI。
    pub(super) fn collect_loaded_descendant_analysis_source_ids(
        &self,
        parent_id: SourceId,
        source_ids: &mut Vec<SourceId>,
    ) {
        for child_id in self.source_registry.child_ids(parent_id).iter().copied() {
            let Some(child) = self.source_registry.node(child_id) else {
                continue;
            };

            if child.kind.is_log_candidate()
                || self.is_source_selectable_for_search_selection(child_id)
            {
                source_ids.push(child_id);
            }

            if child.kind.can_expand() && child.metadata.children_loaded {
                self.collect_loaded_descendant_analysis_source_ids(child_id, source_ids);
            }
        }
    }

    /// 将来源树节点转换为 Jstack 分析目标。
    pub(super) fn jstack_targets_from_source_ids(
        &self,
        source_ids: &[SourceId],
    ) -> Vec<JstackAnalysisTarget> {
        source_ids
            .iter()
            .filter_map(|source_id| {
                let node = self.source_registry.node(*source_id)?;
                if !self.source_supports_jstack_analysis(*source_id) {
                    return None;
                }
                Some(JstackAnalysisTarget {
                    source_id: *source_id,
                    location: node.location.clone(),
                    archive_probe_node: self.jstack_archive_probe_node(*source_id),
                    label: node.label.clone(),
                    path: node.location.display_path(),
                    archive_passwords: self.archive_passwords.clone(),
                })
            })
            .collect()
    }

    /// 为 Jstack 分析生成待探测压缩包快照；已识别日志节点不需要额外探测。
    pub(super) fn jstack_archive_probe_node(&self, source_id: SourceId) -> Option<SourceTreeNode> {
        if !self.is_source_selectable_for_search_selection(source_id) {
            return None;
        }

        let node = self.source_registry.node(source_id)?;
        (!node.kind.is_log_candidate()).then(|| node.clone())
    }

    /// 应用后台 Jstack 分析结果，过期 generation 会被忽略。
    pub(super) fn apply_jstack_analysis_result(
        &mut self,
        analysis_id: usize,
        generation: usize,
        result: JstackAnalysisResult,
    ) {
        let thread_filter = self.jstack_thread_filter();
        let Some(state) = self.jstack_analyses.get_mut(&analysis_id) else {
            return;
        };
        if state.generation != generation {
            return;
        }

        let thread_count = result.thread_count();
        let snapshot_count = result.snapshot_count();
        let skipped_count = result.skipped_count();
        state.task_state = JstackAnalysisTaskState::Ready(result);
        state.rebuild_visible_row_cache(&thread_filter);
        self.placeholder_notice = format!(
            "Jstack 分析完成：{snapshot_count} 个快照，{thread_count} 个线程，跳过 {skipped_count} 个文件"
        );
    }
}
