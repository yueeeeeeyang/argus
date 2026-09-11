//! 文件职责：提取来源树选择、展开折叠、完整加载回填和按节点密码重试等方法到独立子模块。
//! 创建日期：2026-07-08
//! 修改日期：2026-09-07
//! 作者：Argus 开发团队
//! 主要功能：维护来源树语义状态、整树替换式加载回填、加密压缩包子树重试，并通知助手真实来源内容版本变化。

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

    /// 判断来源节点是否是压缩包内目录；分析时直接收集已加载后代文件。
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

    /// 展开或折叠目录、压缩包等来源节点；加密压缩包降级节点转为密码输入引导。
    pub(crate) fn toggle_source_expanded(&mut self, source_id: SourceId, _cx: &mut Context<Self>) {
        let Some(node) = self.source_registry.node(source_id).cloned() else {
            self.placeholder_notice = "未找到可展开来源节点".to_string();
            return;
        };

        if !node.kind.can_expand() {
            self.placeholder_notice = format!("{} 没有可展开的子级", node.label);
            return;
        }

        // 完整初始化后子级始终就绪；只有枚举时被密码拦截的归档节点需要在点击时引导输入密码。
        if matches!(node.kind, SourceKind::Archive(_)) && node.metadata.archive_password_required {
            self.present_archive_password_prompt_for_source_node(&node);
            return;
        }

        if node.metadata.is_loading {
            // 密码重扫进行中的节点允许先行切换展开状态，结果回填时保留用户选择。
            let expanded = if let Some(node) = self.source_registry.node_mut(source_id) {
                node.expanded = !node.expanded;
                node.expanded
            } else {
                return;
            };
            self.source_registry.rebuild_visible_index();
            self.rebuild_filtered_source_ids();
            self.placeholder_notice = if expanded {
                format!("已展开 {}，正在等待密码重扫完成", node.label)
            } else {
                format!("已折叠 {}", node.label)
            };
            return;
        }

        let expanded = self
            .source_registry
            .toggle_expanded(source_id)
            .unwrap_or(false);
        self.rebuild_filtered_source_ids();
        self.placeholder_notice = if expanded {
            format!("已展开 {}", node.label)
        } else {
            format!("已折叠 {}", node.label)
        };
    }

    /// 为加密压缩包降级节点构造缺少密码错误并弹出密码输入框。
    ///
    /// 密码键必须与扫描器读取该节点时使用的容器键一致：本地归档用根键，
    /// 嵌套归档用外层路径加完整容器条目链路，否则输入的密码无法在重扫时命中。
    pub(super) fn present_archive_password_prompt_for_source_node(
        &mut self,
        node: &SourceTreeNode,
    ) {
        let source_label = node.location.display_path();
        let key = match &node.location {
            SourceLocation::LocalPath(path) => ArchivePasswordKey::root(path.clone()),
            SourceLocation::ArchiveEntry {
                archive_path,
                container_entries,
                entry_path,
                ..
            } => {
                let mut container_chain = container_entries.clone();
                container_chain.push(entry_path.clone());
                ArchivePasswordKey::new(archive_path.clone(), &container_chain)
            }
        };
        let error =
            ArchivePasswordError::required(source_label.clone()).with_context(key, source_label);
        self.present_archive_password_prompt(
            error,
            ArchivePasswordRetryAction::ReloadArchiveNode { source_id: node.id },
        );
    }

    /// 密码提交后在后台仅重扫目标压缩包子树；节点保持转圈直到结果回填。
    pub(super) fn start_archive_node_password_retry(
        &mut self,
        source_id: SourceId,
        cx: &mut Context<Self>,
    ) {
        let Some(node) = self.source_registry.node(source_id).cloned() else {
            self.placeholder_notice = "未找到需要密码的压缩包节点".to_string();
            return;
        };

        self.source_registry.set_loading(source_id, true);
        self.source_registry.rebuild_visible_index();
        self.rebuild_filtered_source_ids();
        self.placeholder_notice = format!("正在按密码重新扫描 {}", node.label);

        let loader_config = self.config.loader.clone();
        let archive_passwords = self.archive_passwords.clone();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    SourceTreeScanner::scan_archive_subtree(
                        &node,
                        loader_config,
                        archive_passwords,
                        tokio_util::sync::CancellationToken::new(),
                    )
                })
                .await;

            view.update(cx, |app, cx| {
                app.apply_archive_node_retry_result(source_id, result, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 应用按节点密码重试的扫描结果：成功原子替换子树，密码错误再弹窗，其它错误标记到节点。
    pub(super) fn apply_archive_node_retry_result(
        &mut self,
        source_id: SourceId,
        result: anyhow::Result<SourceTreeScanResult>,
        cx: &mut Context<Self>,
    ) {
        let scan_result = match result {
            Ok(scan_result) => scan_result,
            Err(error) => {
                self.source_registry.set_loading(source_id, false);
                // 密码仍不正确：沿用现有 Invalid 流程清除错误密码缓存后再次弹窗。
                if let Some(password_error) = find_archive_password_error(&error)
                    && self.present_archive_password_prompt(
                        password_error,
                        ArchivePasswordRetryAction::ReloadArchiveNode { source_id },
                    )
                {
                    self.rebuild_filtered_source_ids();
                    return;
                }
                let label = self
                    .source_registry
                    .node(source_id)
                    .map(|node| node.label.clone())
                    .unwrap_or_else(|| "压缩包".to_string());
                if let Some(node) = self.source_registry.node_mut(source_id) {
                    node.metadata.message = Some(error.to_string());
                }
                self.rebuild_filtered_source_ids();
                self.placeholder_notice = format!("重新扫描 {label} 失败：{error}");
                return;
            }
        };

        let Some(&subtree_root_id) = scan_result.registry.root_ids().first() else {
            // 严格重扫成功必然包含子树根；防御性兜底，避免静默吞掉异常结果。
            self.source_registry.set_loading(source_id, false);
            self.placeholder_notice = "压缩包重试扫描未返回有效内容".to_string();
            return;
        };
        let label = self
            .source_registry
            .node(source_id)
            .map(|node| node.label.clone())
            .unwrap_or_else(|| "压缩包".to_string());
        if self
            .source_registry
            .replace_node_subtree_from(source_id, &scan_result.registry, subtree_root_id, true)
            .is_none()
        {
            self.placeholder_notice = "压缩包节点已不在当前来源树中，请重新加载来源".to_string();
            return;
        }
        self.rebuild_filtered_source_ids();
        self.mark_source_content_changed(cx);
        self.placeholder_notice = if scan_result.warnings.is_empty() {
            format!("已解锁并展开 {label}")
        } else {
            format!(
                "已解锁并展开 {label}，{} 项警告：{}",
                scan_result.warnings.len(),
                scan_result.warnings.join("；")
            )
        };
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
    }

    /// 应用整树完整加载结果。
    ///
    /// 每次成功加载真实来源都会替换旧来源，避免不同批次日志结构混在同一棵树中。
    pub(crate) fn apply_load_report(&mut self, result: SourceTreeScanResult) -> bool {
        self.is_source_loading = false;
        let SourceTreeScanResult { registry, warnings } = result;

        if registry.is_empty() {
            self.placeholder_notice = if warnings.is_empty() {
                "未加载任何日志来源".to_string()
            } else {
                format!("来源加载失败：{}", warnings.join("；"))
            };
            return false;
        }

        let added_count = registry.tree_order_source_ids().len();
        self.source_registry = registry;
        self.has_loaded_real_sources = true;
        self.source_picker.selected_paths.clear();
        self.reset_log_workspace_after_source_replace();

        self.placeholder_notice = if warnings.is_empty() {
            format!("已加载 {added_count} 个来源节点，请选择日志")
        } else {
            format!(
                "已加载 {added_count} 个来源节点，{} 项警告：{}",
                warnings.len(),
                warnings.join("；")
            )
        };
        true
    }

    /// 应用后台完整加载结果；过期 generation（已被更新的加载请求取消）直接丢弃。
    pub(crate) fn apply_source_load_result(
        &mut self,
        load_generation: usize,
        result: anyhow::Result<SourceTreeScanResult>,
        cx: &mut Context<Self>,
    ) {
        if self.source_load_generation != load_generation {
            return;
        }
        self.source_load_cancellation = None;
        self.clear_jstack_cell_hover_preview();
        match result {
            Ok(scan_result) => {
                if self.apply_load_report(scan_result) {
                    self.reset_assistant_after_log_reload(cx);
                }
            }
            Err(error) => {
                self.is_source_loading = false;
                self.placeholder_notice = format!("来源加载失败：{error}");
            }
        }
    }
}
