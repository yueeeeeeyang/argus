//! 文件职责：提取来源树选择、展开折叠、完整加载回填和按节点密码重试等方法到独立子模块。
//! 创建日期：2026-07-08
//! 修改日期：2026-10-28
//! 作者：Argus 开发团队
//! 主要功能：维护来源树语义状态、整树替换式加载回填、加密压缩包子树重试和单文件另存为。

use super::*;
use gpui::{PathPromptOptions, SharedString};

impl ArgusApp {
    /// 判断来源节点是否是可直接另存为的单个文件。
    ///
    /// 说明：物化后所有来源都是工作目录中的普通文件，目录、加密占位与不支持的节点都不具备
    /// 单文件语义，因此只有 `LogFile` 且确实指向普通文件时才允许另存。
    pub(super) fn source_is_exportable_file(&self, source_id: SourceId) -> bool {
        self.source_registry.node(source_id).is_some_and(|node| {
            node.kind.is_log_candidate()
                && matches!(&node.location, SourceLocation::LocalPath(path) if path.is_file())
        })
    }

    /// 打开目标目录选择器，把选中的来源文件另存到该目录。
    ///
    /// 参数说明：
    /// - `source_id`：来源树中被右键的文件节点 ID。
    /// - `cx`：主应用上下文，用于在后台执行复制并把结果写回界面。
    ///
    /// 返回值：无；节点不是单个文件时只更新提示，不做任何文件操作。
    pub(crate) fn save_source_as(&mut self, source_id: SourceId, cx: &mut Context<Self>) {
        let Some(node) = self.source_registry.node(source_id) else {
            self.placeholder_notice = "未找到可另存的来源节点".to_string();
            return;
        };
        if !self.source_is_exportable_file(source_id) {
            self.placeholder_notice = format!("{} 不是可另存的单个文件", node.label);
            return;
        }
        // 物化后来源位置只有本地路径这一种形态，无需再做分支处理。
        let SourceLocation::LocalPath(source_path) = node.location.clone();
        let label = node.label.clone();

        let receiver = {
            let app_context: &gpui::App = (*cx).borrow();
            app_context.prompt_for_paths(PathPromptOptions {
                files: false,
                directories: true,
                multiple: false,
                prompt: Some(SharedString::from("选择另存为的目标目录")),
            })
        };
        cx.spawn(async move |view, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(target_dir) = paths.into_iter().next() else {
                return;
            };
            view.update(cx, |app, cx| {
                app.start_source_export(source_path, target_dir, label, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 在后台把来源文件复制到目标目录，并在完成后报告实际写入位置。
    pub(super) fn start_source_export(
        &mut self,
        source_path: std::path::PathBuf,
        target_dir: std::path::PathBuf,
        label: String,
        cx: &mut Context<Self>,
    ) {
        self.placeholder_notice = format!("正在另存 {label}...");
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    crate::loader::export_source_file(&source_path, &target_dir)
                        .map_err(|error| format!("{error:#}"))
                })
                .await;

            view.update(cx, |app, cx| {
                app.placeholder_notice = match result {
                    Ok(outcome) => {
                        let size = crate::utils::size_format::format_bytes(outcome.copied_bytes);
                        if outcome.used_fallback_name {
                            format!(
                                "已另存为 {}（目标目录存在同名文件，已自动改用新文件名，{size}）",
                                outcome.target_path.display()
                            )
                        } else {
                            format!("已另存为 {}（{size}）", outcome.target_path.display())
                        }
                    }
                    Err(error) => format!("另存 {label} 失败：{error}"),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 判断来源节点是否至少支持一种右键分析动作。
    pub(super) fn source_supports_any_analysis_context_menu(&self, source_id: SourceId) -> bool {
        self.source_supports_jstack_analysis(source_id)
            || self.source_supports_runtime_analysis(source_id)
    }

    /// 判断来源节点是否是分析功能可以展开收集的目录。
    pub(super) fn source_is_analysis_directory(&self, source_id: SourceId) -> bool {
        self.source_registry
            .node(source_id)
            .is_some_and(|node| matches!(node.kind, SourceKind::Directory))
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
        if self.is_source_tree_search_open {
            self.close_source_tree_search();
        }
        let Some(node) = self.source_registry.node(source_id).cloned() else {
            self.placeholder_notice = "未找到可展开来源节点".to_string();
            return;
        };

        // 加密压缩包占位节点没有可展开内容，点击时引导输入密码后追加物化。
        if matches!(node.kind, SourceKind::ArchivePasswordRequired) {
            self.present_archive_password_prompt_for_source_node(&node);
            return;
        }

        if !node.kind.can_expand() {
            self.placeholder_notice = format!("{} 没有可展开的子级", node.label);
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

    /// 为加密压缩包占位节点构造缺少密码错误并弹出密码输入框。
    ///
    /// 占位节点只来自最外层加密压缩包，密码键固定为根键，保证输入的密码在追加物化时命中。
    pub(super) fn present_archive_password_prompt_for_source_node(
        &mut self,
        node: &SourceTreeNode,
    ) {
        let source_label = node.location.display_path();
        let SourceLocation::LocalPath(path) = &node.location;
        let key = ArchivePasswordKey::root(path.clone());
        let error =
            ArchivePasswordError::required(source_label.clone()).with_context(key, source_label);
        self.present_archive_password_prompt(
            error,
            ArchivePasswordRetryAction::ReloadArchiveNode { source_id: node.id },
        );
    }

    /// 密码提交后在后台向当前工作目录追加物化目标压缩包并重扫其顶层目录；节点保持转圈直到结果回填。
    pub(super) fn start_archive_node_password_retry(
        &mut self,
        source_id: SourceId,
        cx: &mut Context<Self>,
    ) {
        let Some(node) = self.source_registry.node(source_id).cloned() else {
            self.placeholder_notice = "未找到需要密码的压缩包节点".to_string();
            return;
        };
        let SourceLocation::LocalPath(archive_path) = node.location.clone();
        let Some(workspace_root) = self.source_workspace_root.clone() else {
            self.placeholder_notice = "日志工作目录已失效，请重新加载来源后重试".to_string();
            return;
        };

        self.source_registry.set_loading(source_id, true);
        self.source_registry.rebuild_visible_index();
        self.rebuild_filtered_source_ids();
        self.placeholder_notice = format!("正在按密码解压 {}到工作目录", node.label);

        let loader_config = self.config.loader.clone();
        let archive_passwords = self.archive_passwords.clone();
        let label = node.label.clone();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let root_info = crate::loader::workspace::append_materialize_archive(
                        &workspace_root,
                        &label,
                        &archive_path,
                        &loader_config,
                        &archive_passwords,
                        &tokio_util::sync::CancellationToken::new(),
                    )?;
                    SourceTreeScanner::scan_paths(
                        vec![root_info.path],
                        loader_config,
                        tokio_util::sync::CancellationToken::new(),
                        None,
                    )
                })
                .await;

            view.update(cx, |app, cx| {
                app.apply_archive_node_retry_result(source_id, result);
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
        let retained_connection_tabs = self
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
        let previous_active_tab_id = self.active_tab_id;
        self.tabs = retained_connection_tabs;
        // 没有选择日志时不再补占位标签：保留原激活页签（若仍存在），否则激活最后一个连接页签。
        self.active_tab_id = previous_active_tab_id
            .filter(|tab_id| self.tabs.iter().any(|tab| tab.id == *tab_id))
            .or_else(|| self.tabs.last().map(|tab| tab.id));

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
        self.source_load_progress = None;
        let SourceTreeScanResult { registry, warnings } = result;

        if registry.is_empty() {
            self.placeholder_notice = if warnings.is_empty() {
                "未加载任何日志来源".to_string()
            } else {
                format!("来源加载失败：{}", warnings.join("；"))
            };
            self.source_load_notice = Some(self.placeholder_notice.clone());
            return false;
        }

        let added_count = registry.tree_order_source_ids().len();
        self.source_registry = registry;
        // 加载后自动展开单链目录，用户无需逐级点开只含一个子目录的嵌套路径。
        self.source_registry.expand_single_child_directory_chain();
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
        // 部分文件被跳过时保留提示，让用户知道来源不完整；完全成功则清空。
        self.source_load_notice = Self::summarize_load_warnings(&warnings);
        true
    }

    /// 生成来源加载警告摘要；提示条空间有限，最多列出前 3 项。
    ///
    /// 参数说明：
    /// - `warnings`：物化与扫描阶段累计的警告文案。
    ///
    /// 返回值：无警告时返回 `None`，否则返回带跳过数量的摘要。
    fn summarize_load_warnings(warnings: &[String]) -> Option<String> {
        if warnings.is_empty() {
            return None;
        }
        let preview = warnings
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join("；");
        if warnings.len() > 3 {
            Some(format!(
                "{} 项文件已跳过：{preview}；…（其余 {} 项）",
                warnings.len(),
                warnings.len() - 3
            ))
        } else {
            Some(format!("{} 项文件已跳过：{preview}", warnings.len()))
        }
    }

    /// 应用来源加载后的侧栏默认状态：单文件来源没有目录层级，收起左侧目录树并直接打开
    /// 该文件并触发后台读取，不再要求二次点击；多文件或目录来源则展开目录树，
    /// 避免上一次单文件加载留下的收起状态让用户看不到新加载的来源。
    fn apply_source_load_panel_defaults(&mut self, cx: &mut Context<Self>) {
        let root_ids = self.source_registry.root_ids();
        let single_file_root = if root_ids.len() == 1 {
            root_ids.first().copied()
        } else {
            None
        };
        let single_file_root = single_file_root.filter(|root_id| {
            self.source_registry
                .node(*root_id)
                .is_some_and(|node| matches!(node.kind, SourceKind::LogFile))
        });
        let Some(root_id) = single_file_root else {
            if self.is_source_panel_collapsed {
                self.is_source_panel_collapsed = false;
                self.sync_source_panel_animation_to_current_width();
            }
            return;
        };
        if !self.is_source_panel_collapsed {
            self.is_source_panel_collapsed = true;
            self.sync_source_panel_animation_to_current_width();
        }
        self.select_source(root_id);
        self.request_open_log_content(root_id, cx);
    }

    /// 应用后台完整加载结果；过期 generation（已被更新的加载请求取消）直接丢弃。
    pub(crate) fn apply_source_load_result(
        &mut self,
        load_generation: usize,
        result: anyhow::Result<(crate::loader::MaterializedWorkspace, SourceTreeScanResult)>,
        cx: &mut Context<Self>,
    ) {
        if self.source_load_generation != load_generation {
            // 过期加载结果直接丢弃，附带的新工作目录一并回滚删除。
            if let Ok((workspace, _)) = result {
                crate::loader::workspace::delete_workspace_best_effort(workspace.root);
            }
            return;
        }
        self.source_load_cancellation = None;
        self.source_load_progress = None;
        self.clear_jstack_cell_hover_preview();
        match result {
            Ok((mut workspace, mut scan_result)) => {
                // 物化警告并入加载报告，让用户在完成提示中看到跳过和降级项。
                scan_result.warnings.append(&mut workspace.warnings);
                if self.apply_load_report(scan_result) {
                    self.apply_source_load_panel_defaults(cx);
                    self.placeholder_notice = format!(
                        "{}，已物化 {} 个文件到工作目录",
                        self.placeholder_notice, workspace.materialized_files
                    );
                    self.source_workspace_root = Some(workspace.root);
                } else {
                    // 未加载到有效来源时新建工作目录没有消费者，后台回滚删除。
                    crate::loader::workspace::delete_workspace_best_effort(workspace.root);
                }
            }
            Err(error) => {
                self.is_source_loading = false;
                self.placeholder_notice = format!("来源加载失败：{error:#}");
                self.source_load_notice = Some(self.placeholder_notice.clone());
                // 加载失败会回到没有标签的欢迎态，错误在欢迎面板展示。
                self.sync_source_panel_welcome_state();
            }
        }
    }
}
