//! 文件职责：提取来源树选择、展开折叠、子级懒加载、压缩包探测和分析能力判定等方法到独立子模块。

use super::*;

impl ArgusApp {
    /// 判断来源节点是否至少支持一种右键分析动作。
    pub(super) fn source_supports_any_analysis_context_menu(&self, source_id: SourceId) -> bool {
        self.source_supports_jstack_analysis(source_id)
            || self.source_supports_runtime_analysis(source_id)
    }

    /// 判断来源节点是否是分析功能可以展开收集的目录。
    pub(super) fn source_is_analysis_directory(&self, source_id: SourceId) -> bool {
        self.source_registry.node(source_id).is_some_and(|node| {
            matches!(
                node.kind,
                SourceKind::Directory | SourceKind::ArchiveDirectory
            )
        })
    }

    /// 判断来源节点是否是本地真实目录；本地目录可以直接交给后台递归文件系统。
    pub(super) fn source_is_local_directory(&self, source_id: SourceId) -> bool {
        self.source_registry.node(source_id).is_some_and(|node| {
            matches!(node.kind, SourceKind::Directory)
                && matches!(node.location, SourceLocation::LocalPath(_))
        })
    }

    /// 判断来源节点是否是压缩包内目录；需要先加载子级再收集已加载后代文件。
    pub(super) fn source_is_archive_directory(&self, source_id: SourceId) -> bool {
        self.source_registry
            .node(source_id)
            .is_some_and(|node| matches!(node.kind, SourceKind::ArchiveDirectory))
    }

    /// 判断来源节点是否支持 Jstack 线程日志分析入口。
    pub(super) fn source_supports_jstack_analysis(&self, source_id: SourceId) -> bool {
        self.is_source_selectable_for_search_selection(source_id)
            || self.source_is_analysis_directory(source_id)
    }

    /// 判断来源节点是否支持 Runtime 日志解析入口。
    pub(super) fn source_supports_runtime_analysis(&self, source_id: SourceId) -> bool {
        self.source_registry.node(source_id).is_some_and(|node| {
            node.kind.is_log_candidate()
                || self.is_source_selectable_for_search_selection(source_id)
                || self.source_is_analysis_directory(source_id)
        })
    }

    /// 确保压缩包内目录子级已经加载；未加载时先触发加载并记录待续做动作。
    pub(super) fn ensure_source_directory_ready_for_analysis(
        &mut self,
        source_id: SourceId,
        pending_action: PendingSourceAnalysisAction,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.source_is_archive_directory(source_id) {
            return true;
        }

        let Some(node) = self.source_registry.node(source_id).cloned() else {
            self.placeholder_notice = "未找到可分析的来源目录".to_string();
            return false;
        };
        if node.metadata.children_loaded {
            return true;
        }

        self.pending_source_analysis_after_load = Some(pending_action);
        if node.metadata.is_loading {
            self.placeholder_notice = format!("正在加载 {} 的子级，完成后自动开始分析", node.label);
            return false;
        }

        self.start_source_child_load(source_id, node.clone(), cx);
        self.placeholder_notice = format!("正在加载 {} 的子级，完成后自动开始分析", node.label);
        false
    }

    /// 根据节点 ID 选择来源树节点。
    pub(crate) fn select_source(&mut self, source_id: SourceId) {
        let Some(selected_node) = self.source_registry.select(source_id) else {
            self.placeholder_notice = "未找到来源节点".to_string();
            return;
        };

        if selected_node.kind.is_log_candidate() {
            self.open_or_focus_log_tab(source_id);
        } else {
            self.placeholder_notice = format!("已选择来源节点 {}", selected_node.label);
        }
    }

    /// 展开或折叠目录、压缩包等来源节点。
    pub(crate) fn toggle_source_expanded(&mut self, source_id: SourceId, cx: &mut Context<Self>) {
        let Some(node) = self.source_registry.node(source_id).cloned() else {
            self.placeholder_notice = "未找到可展开来源节点".to_string();
            return;
        };

        if !node.kind.can_expand() {
            self.placeholder_notice = format!("{} 没有可展开的子级", node.label);
            return;
        }

        if node.metadata.is_loading {
            if let Some(node) = self.source_registry.node_mut(source_id) {
                node.expanded = !node.expanded;
            }
            self.source_registry.rebuild_visible_index();
            self.rebuild_filtered_source_ids();
            self.placeholder_notice = if node.expanded {
                format!("已折叠 {}，后台加载完成后保持收起", node.label)
            } else {
                format!("已展开 {}，正在等待后台加载完成", node.label)
            };
            return;
        }

        if node.expanded {
            self.source_registry.toggle_expanded(source_id);
            self.rebuild_filtered_source_ids();
            self.placeholder_notice = format!("已折叠 {}", node.label);
            return;
        }

        if node.metadata.children_loaded {
            self.source_registry.toggle_expanded(source_id);
            self.rebuild_filtered_source_ids();
            self.placeholder_notice = format!("已展开 {}", node.label);
            return;
        }

        if matches!(node.kind, SourceKind::Archive(_))
            && !self.source_archive_probe_completed_ids.contains(&source_id)
        {
            let label = node.label.clone();
            self.start_direct_source_archive_probe(source_id, node, cx);
            self.placeholder_notice = format!("正在识别 {label}，完成后继续打开或展开");
            return;
        }

        self.start_source_child_load(source_id, node, cx);
    }

    /// 启动指定可展开节点的子级后台加载。
    pub(super) fn start_source_child_load(
        &mut self,
        source_id: SourceId,
        node: SourceTreeNode,
        cx: &mut Context<Self>,
    ) {
        if !node.kind.can_expand() || node.metadata.children_loaded || node.metadata.is_loading {
            return;
        }

        if let Some(node) = self.source_registry.node_mut(source_id) {
            node.expanded = true;
            node.metadata.is_loading = true;
        }
        self.source_registry.rebuild_visible_index();
        self.rebuild_filtered_source_ids();
        self.placeholder_notice = format!("正在加载 {} 的子级", node.label);

        let loader_config = self.config.loader.clone();
        let archive_passwords = self.archive_passwords.clone();
        let load_generation = self.next_source_child_load_generation(source_id);
        cx.spawn(async move |view, cx| {
            let report = cx
                .background_executor()
                .spawn(async move {
                    LogSourceLoader::new(loader_config)
                        .with_archive_passwords(archive_passwords)
                        .with_deferred_archive_probe()
                        .load_children(&node)
                })
                .await;

            view.update(cx, |app, cx| {
                app.apply_child_load_report_with_context(source_id, load_generation, report, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 收起来源目录树中的所有可展开节点。
    pub(crate) fn collapse_all_sources(&mut self) {
        let collapsed_count = self.source_registry.collapse_all();
        self.rebuild_filtered_source_ids();

        self.placeholder_notice = if collapsed_count == 0 {
            "目录树已处于全部收起状态".to_string()
        } else {
            format!("已收起 {collapsed_count} 个目录树节点")
        };
    }

    /// 返回当前应渲染的来源节点 ID 列表。
    pub(crate) fn visible_source_ids(&self) -> &[SourceId] {
        if self.is_source_tree_filtering() {
            &self.filtered_source_ids
        } else {
            self.source_registry.visible_source_ids()
        }
    }

    /// 清理旧日志工作区状态，确保新来源不会继承旧日志的标签、筛选和内容选择。
    pub(super) fn reset_log_workspace_after_source_replace(&mut self) {
        self.log_read_states.clear();
        self.log_reader_generations.clear();
        self.log_tab_view_states.clear();
        self.jstack_analyses.clear();
        self.next_jstack_analysis_id = 1;
        self.runtime_analyses.clear();
        self.next_runtime_analysis_id = 1;
        self.reset_log_text_selection();
        self.log_scrollbar_drag = None;
        self.reset_log_search_runtime_state();
        self.hovered_tab_id = None;
        self.active_menu = None;
        self.log_scrollbar_drag = None;
        self.tab_menu_scroll = UniformListScrollHandle::new();

        // 日志来源替换只影响日志分析域；SSH 终端和远程文件管理会话继续保留，
        // 方便用户加载日志后仍能通过原页签返回正在进行的连接工作。
        let mut retained_connection_tabs = self
            .tabs
            .iter()
            .filter(|tab| {
                matches!(
                    tab.kind,
                    TabKind::SshTerminal { .. } | TabKind::RemoteFileManager { .. }
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        let empty_tab_id = if retained_connection_tabs.is_empty() {
            self.next_tab_id = 2;
            1
        } else {
            let tab_id = self.next_tab_id;
            self.next_tab_id += 1;
            tab_id
        };
        retained_connection_tabs.push(ArgusTab {
            id: empty_tab_id,
            title: "未选择日志".to_string(),
            kind: TabKind::Empty,
        });
        self.tabs = retained_connection_tabs;
        self.active_tab_id = empty_tab_id;
        self.ensure_log_tab_view_state(empty_tab_id);

        self.is_source_tree_search_open = false;
        self.source_tree_search_input.value.clear();
        self.source_tree_search_input.cursor = 0;
        self.source_tree_search_input.selection_anchor = None;
        self.source_tree_search_input.selection_drag = None;
        self.source_tree_search_input.is_focused = false;
        self.filtered_source_ids.clear();
        self.source_tree_scroll
            .scroll_to_item(0, ScrollStrategy::Top);
        self.pending_source_analysis_after_load = None;
    }

    /// 应用根来源加载报告。
    ///
    /// 每次成功加载真实来源都会替换旧来源，避免不同批次日志结构混在同一棵树中。
    pub(crate) fn apply_load_report(&mut self, report: LoadReport) {
        self.is_source_loading = false;
        let added_count = report.added_count;

        if report.registry.is_empty() {
            self.placeholder_notice = if report.errors.is_empty() {
                "未加载任何日志来源".to_string()
            } else {
                format!("来源加载失败：{}", report.errors.join("；"))
            };
            return;
        }

        self.source_registry = report.registry;
        self.has_loaded_real_sources = true;
        self.source_child_load_generations.clear();
        self.clear_source_archive_probe_state();
        self.source_picker.selected_paths.clear();
        self.reset_log_workspace_after_source_replace();
        let probe_ids = self.source_registry.tree_order_source_ids().to_vec();
        self.enqueue_source_archive_probe_ids(&probe_ids, false);

        self.placeholder_notice = if report.errors.is_empty() {
            format!("已加载 {added_count} 个来源节点，请选择日志")
        } else {
            format!(
                "已加载 {added_count} 个来源节点，{} 项失败：{}",
                report.errors.len(),
                report.errors.join("；")
            )
        };
    }

    /// 在 UI 事件中应用根来源加载报告，并同步清理 Jstack 方块悬浮气泡。
    pub(crate) fn apply_load_report_with_context(
        &mut self,
        report: LoadReport,
        retry_action: Option<crate::app::ArchivePasswordRetryAction>,
        _cx: &mut Context<Self>,
    ) {
        self.clear_jstack_cell_hover_preview();
        if let Some(password_error) = report.password_request.clone()
            && let Some(retry_action) = retry_action
            && self.present_archive_password_prompt(password_error, retry_action)
        {
            self.is_source_loading = false;
            return;
        }
        self.apply_load_report(report);
    }

    /// 应用懒加载子级报告，并挂回指定父节点。
    #[cfg(test)]
    pub(super) fn apply_child_load_report(
        &mut self,
        parent_id: SourceId,
        load_generation: usize,
        report: LoadReport,
    ) {
        self.apply_child_load_report_internal(parent_id, load_generation, report);
    }

    /// 在 UI 回调中应用子级加载报告，并在压缩包目录加载完毕后自动续做分析动作。
    pub(crate) fn apply_child_load_report_with_context(
        &mut self,
        parent_id: SourceId,
        load_generation: usize,
        report: LoadReport,
        cx: &mut Context<Self>,
    ) {
        if self.apply_child_load_report_internal(parent_id, load_generation, report) {
            self.resume_pending_source_analysis(parent_id, cx);
        }
    }

    /// 应用懒加载子级报告的共享实现，返回是否处理了当前有效 generation。
    pub(super) fn apply_child_load_report_internal(
        &mut self,
        parent_id: SourceId,
        load_generation: usize,
        report: LoadReport,
    ) -> bool {
        if self.source_child_load_generations.get(&parent_id).copied() != Some(load_generation) {
            return false;
        }

        if let Some(password_error) = report.password_request.clone()
            && self.present_archive_password_prompt(
                password_error,
                crate::app::ArchivePasswordRetryAction::LoadChildren {
                    source_id: parent_id,
                },
            )
        {
            self.source_child_load_generations.remove(&parent_id);
            if let Some(parent) = self.source_registry.node_mut(parent_id) {
                parent.metadata.is_loading = false;
                parent.metadata.message = Some("等待输入压缩包密码".to_string());
            }
            self.rebuild_filtered_source_ids();
            return true;
        }
        self.source_child_load_generations.remove(&parent_id);

        if report.registry.is_empty() && !report.errors.is_empty() {
            let message = report.errors.join("；");
            self.source_registry
                .mark_children_load_failed(parent_id, message.clone());
            self.rebuild_filtered_source_ids();
            self.placeholder_notice = format!("子级加载失败：{message}");
            return true;
        }

        let should_keep_expanded = self
            .source_registry
            .node(parent_id)
            .map(|node| node.expanded)
            .unwrap_or(false);
        let added_count = self.source_registry.append_children_registry(
            parent_id,
            report.registry,
            should_keep_expanded,
        );

        if let Some(parent) = self.source_registry.node_mut(parent_id) {
            if report.errors.is_empty() {
                parent.metadata.message = None;
            } else {
                parent.metadata.message = Some(report.errors.join("；"));
            }
        }
        self.rebuild_filtered_source_ids();
        let child_ids = self.source_registry.child_ids(parent_id).to_vec();
        self.enqueue_source_archive_probe_ids(&child_ids, false);

        self.placeholder_notice = if report.errors.is_empty() {
            format!("已加载 {added_count} 个子节点")
        } else if added_count == 0 {
            format!("子级加载失败：{}", report.errors.join("；"))
        } else {
            format!(
                "已加载 {added_count} 个子节点，{} 项失败：{}",
                report.errors.len(),
                report.errors.join("；")
            )
        };
        true
    }

    /// 子级加载成功返回后续做被挂起的分析动作，避免用户二次右键。
    pub(super) fn resume_pending_source_analysis(
        &mut self,
        parent_id: SourceId,
        cx: &mut Context<Self>,
    ) {
        let Some(action) = self.pending_source_analysis_after_load else {
            return;
        };
        if action.source_id() != parent_id {
            return;
        }

        self.pending_source_analysis_after_load = None;
        if !self
            .source_registry
            .node(parent_id)
            .is_some_and(|node| node.metadata.children_loaded)
        {
            return;
        }

        match action {
            PendingSourceAnalysisAction::Jstack { source_id } => {
                self.open_jstack_analysis_tab(source_id, cx);
            }
            PendingSourceAnalysisAction::Runtime { source_id } => {
                self.open_runtime_analysis_tab(source_id, cx);
            }
        }
    }

    /// 清理来源树压缩包探测队列；来源树整体替换时调用。
    pub(super) fn clear_source_archive_probe_state(&mut self) {
        self.source_archive_probe_queue.clear();
        self.source_archive_probe_queued_ids.clear();
        self.source_archive_probe_inflight_ids.clear();
        self.source_archive_probe_direct_inflight_ids.clear();
        self.source_archive_probe_completed_ids.clear();
        self.source_archive_probe_click_intents.clear();
        self.source_archive_probe_generation = self.source_archive_probe_generation.wrapping_add(1);
        self.pending_source_analysis_after_load = None;
    }

    /// 将可见来源节点提升到压缩包探测队列前端。
    pub(crate) fn prioritize_visible_source_archive_probes(
        &mut self,
        source_ids: &[SourceId],
        cx: &mut Context<Self>,
    ) {
        self.enqueue_source_archive_probes(source_ids, true, cx);
    }

    /// 入队来源树压缩包探测任务，支持普通追加和高优先级前插。
    pub(super) fn enqueue_source_archive_probes(
        &mut self,
        source_ids: &[SourceId],
        priority: bool,
        cx: &mut Context<Self>,
    ) {
        if !self.enqueue_source_archive_probe_ids(source_ids, priority) {
            return;
        }

        self.pump_source_archive_probe_queue(cx);
    }

    /// 只把来源压缩包节点放入后台探测队列，不立即启动后台任务。
    pub(super) fn enqueue_source_archive_probe_ids(
        &mut self,
        source_ids: &[SourceId],
        priority: bool,
    ) -> bool {
        let mut accepted_ids = Vec::new();
        for source_id in source_ids.iter().copied() {
            if !self.should_probe_source_archive(source_id) {
                continue;
            }
            accepted_ids.push(source_id);
        }

        if accepted_ids.is_empty() {
            return false;
        }

        if priority {
            for source_id in accepted_ids.into_iter().rev() {
                if self.source_archive_probe_queued_ids.contains(&source_id) {
                    self.source_archive_probe_queue
                        .retain(|queued_id| *queued_id != source_id);
                } else {
                    self.source_archive_probe_queued_ids.insert(source_id);
                }
                self.source_archive_probe_queue.push_front(source_id);
            }
        } else {
            for source_id in accepted_ids {
                if self.source_archive_probe_queued_ids.insert(source_id) {
                    self.source_archive_probe_queue.push_back(source_id);
                }
            }
        }
        true
    }

    /// 判断来源节点是否需要后台单文件压缩包探测。
    pub(super) fn should_probe_source_archive(&self, source_id: SourceId) -> bool {
        if self.source_archive_probe_completed_ids.contains(&source_id)
            || self.source_archive_probe_inflight_ids.contains(&source_id)
            || self
                .source_archive_probe_direct_inflight_ids
                .contains(&source_id)
        {
            return false;
        }

        self.source_registry.node(source_id).is_some_and(|node| {
            matches!(node.kind, SourceKind::Archive(_)) && !node.metadata.children_loaded
        })
    }

    /// 用户直接点击未探测压缩包时，绕过批量队列立即启动单节点探测。
    pub(super) fn start_direct_source_archive_probe(
        &mut self,
        source_id: SourceId,
        node: SourceTreeNode,
        cx: &mut Context<Self>,
    ) {
        self.source_archive_probe_click_intents.insert(source_id);
        if self.source_archive_probe_queued_ids.remove(&source_id) {
            self.source_archive_probe_queue
                .retain(|queued_id| *queued_id != source_id);
        }

        if self
            .source_archive_probe_direct_inflight_ids
            .contains(&source_id)
            || self.source_archive_probe_inflight_ids.contains(&source_id)
        {
            return;
        }

        self.source_archive_probe_direct_inflight_ids
            .insert(source_id);
        let loader_config = self.config.loader.clone();
        let archive_passwords = self.archive_passwords.clone();
        let generation = self.source_archive_probe_generation;
        let request = SourceArchiveProbeRequest { source_id, node };

        cx.spawn(async move |view, cx| {
            let results = cx
                .background_executor()
                .spawn(async move {
                    LogSourceLoader::new(loader_config)
                        .with_archive_passwords(archive_passwords)
                        .probe_archive_nodes(vec![request])
                })
                .await;

            view.update(cx, |app, cx| {
                app.apply_source_archive_probe_results(generation, results, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 按批次启动后台压缩包探测；每批完成后会再次调用自身处理后续队列。
    pub(super) fn pump_source_archive_probe_queue(&mut self, cx: &mut Context<Self>) {
        if !self.source_archive_probe_inflight_ids.is_empty() {
            return;
        }

        let batch_size = self
            .config
            .loader
            .archive_probe_concurrency
            .clamp(1, 16)
            .saturating_mul(SOURCE_ARCHIVE_PROBE_BATCH_FACTOR)
            .max(1);
        let mut batch_ids = Vec::new();
        while batch_ids.len() < batch_size {
            let Some(source_id) = self.source_archive_probe_queue.pop_front() else {
                break;
            };
            self.source_archive_probe_queued_ids.remove(&source_id);
            if !self.should_probe_source_archive(source_id) {
                continue;
            }
            self.source_archive_probe_inflight_ids.insert(source_id);
            batch_ids.push(source_id);
        }

        if batch_ids.is_empty() {
            return;
        }

        let requests = batch_ids
            .iter()
            .filter_map(|source_id| {
                self.source_registry.node(*source_id).cloned().map(|node| {
                    SourceArchiveProbeRequest {
                        source_id: *source_id,
                        node,
                    }
                })
            })
            .collect::<Vec<_>>();

        if requests.is_empty() {
            for source_id in batch_ids {
                self.source_archive_probe_inflight_ids.remove(&source_id);
            }
            return;
        }

        let loader_config = self.config.loader.clone();
        let archive_passwords = self.archive_passwords.clone();
        let generation = self.source_archive_probe_generation;
        cx.spawn(async move |view, cx| {
            let results = cx
                .background_executor()
                .spawn(async move {
                    LogSourceLoader::new(loader_config)
                        .with_archive_passwords(archive_passwords)
                        .probe_archive_nodes(requests)
                })
                .await;

            view.update(cx, |app, cx| {
                app.apply_source_archive_probe_results(generation, results, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 批量应用后台单文件压缩包探测结果。
    pub(super) fn apply_source_archive_probe_results(
        &mut self,
        generation: usize,
        results: Vec<SourceArchiveProbeResult>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.source_archive_probe_generation {
            return;
        }

        let mut changed_count = 0;
        let mut fallback_expand_ids = Vec::new();
        let mut open_log_ids = Vec::new();

        for result in results {
            let was_completed = self
                .source_archive_probe_completed_ids
                .contains(&result.source_id);
            self.source_archive_probe_inflight_ids
                .remove(&result.source_id);
            self.source_archive_probe_direct_inflight_ids
                .remove(&result.source_id);
            self.source_archive_probe_completed_ids
                .insert(result.source_id);
            let had_click_intent = self
                .source_archive_probe_click_intents
                .remove(&result.source_id);

            if was_completed && !had_click_intent {
                continue;
            }

            if let Some(patch) = result.patch {
                let replaced = self.source_registry.replace_node_payload(
                    result.source_id,
                    patch.kind,
                    patch.location,
                    patch.metadata,
                );
                if replaced {
                    changed_count += 1;
                    if had_click_intent {
                        open_log_ids.push(result.source_id);
                    }
                }
            } else if had_click_intent {
                fallback_expand_ids.push(result.source_id);
            }
        }

        if changed_count > 0 {
            self.source_registry.rebuild_all_indices();
            self.rebuild_filtered_source_ids();
        }

        for source_id in open_log_ids {
            self.select_source(source_id);
            self.request_open_log_content(source_id, cx);
            self.scroll_source_into_view(source_id);
        }

        for source_id in fallback_expand_ids {
            if let Some(node) = self.source_registry.node(source_id).cloned() {
                self.start_source_child_load(source_id, node, cx);
            }
        }

        self.pump_source_archive_probe_queue(cx);
    }

    /// 为指定来源节点生成下一次子级懒加载 generation。
    pub(super) fn next_source_child_load_generation(&mut self, source_id: SourceId) -> usize {
        let generation = self
            .source_child_load_generations
            .entry(source_id)
            .or_insert(0);
        *generation = generation.wrapping_add(1);
        *generation
    }
}
