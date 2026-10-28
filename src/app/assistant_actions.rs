//! 文件职责：维护智能助手标签页的创建、模型配置同步和设置入口。
//! 创建日期：2026-10-28
//! 修改日期：2026-10-28
//! 作者：Argus 开发团队
//! 主要功能：每次点击入口创建独立助手页签，并在模型配置保存后同步给所有会话。

use gpui::{AppContext, Context, Entity, UniformListScrollHandle, Window, point, px};

use crate::app::{ArgusApp, ArgusTab, SettingsSection, TabKind};
use crate::loader::{SourceId, SourceKind, SourceLocation};
use crate::ui::assistant_tab::AssistantTab;
use crate::ui::components::context_menu::{
    ActiveMenu, ActiveMenuKind, MENU_ROW_HEIGHT, MenuAction, SOURCE_TREE_CONTEXT_MENU_WIDTH,
};

impl ArgusApp {
    /// 新建一个独立的智能助手标签页。
    ///
    /// 每次点击入口都会创建新会话，不复用已有标签；页面初始为空闲状态，不会自动
    /// 触发来源扫描或模型请求。
    pub(crate) fn open_assistant_tab(&mut self, cx: &mut Context<Self>) {
        let session_id = self.next_tab_id;
        let app = cx.entity();
        let theme = self.theme.clone();
        let ai_config = self.config.ai.clone();
        let assistant_tab = cx.new(|tab_cx| AssistantTab::new(app, theme, ai_config, tab_cx));
        self.assistant_tabs.insert(session_id, assistant_tab);
        self.tabs.push(ArgusTab {
            id: session_id,
            title: self.next_assistant_tab_title(),
            kind: TabKind::Assistant { session_id },
        });
        self.next_tab_id += 1;
        self.active_tab_id = Some(session_id);
        self.close_log_search_results_for_active_non_log_tab();
        self.placeholder_notice = "已打开智能助手，输入问题开始对话".to_string();
    }

    /// 返回下一个可用的智能助手页签名称；优先复用已关闭页签留下的序号空位。
    fn next_assistant_tab_title(&self) -> String {
        let mut index = 1usize;
        loop {
            let title = assistant_tab_title(index);
            let is_used = self
                .tabs
                .iter()
                .any(|tab| matches!(tab.kind, TabKind::Assistant { .. }) && tab.title == title);
            if !is_used {
                return title;
            }
            index += 1;
        }
    }

    /// 把最新模型配置同步到所有助手标签页。
    ///
    /// 已存在的会话保留对话历史，只更新模型列表、当前选择和凭据检查结果。
    pub(crate) fn refresh_assistant_model_configuration(&mut self, cx: &mut Context<Self>) {
        let config = self.config.ai.clone();
        for assistant_tab in self.assistant_tabs.values() {
            assistant_tab.update(cx, |tab, tab_cx| {
                tab.apply_model_configuration(config.clone());
                tab_cx.notify();
            });
        }
    }

    /// 从助手页入口直接打开模型配置分类。
    pub(crate) fn open_assistant_model_settings(&mut self, cx: &mut Context<Self>) {
        self.selected_settings_section = SettingsSection::AiModel;
        self.open_settings_modal(cx);
    }

    /// 打开来源树的智能助手选择二级菜单，锚定在“添加到智能助手”条目右侧。
    pub(crate) fn open_assistant_picker_menu(&mut self, source_id: SourceId) {
        let Some(active_menu) = self.active_menu.as_ref() else {
            return;
        };
        if !matches!(active_menu.kind, ActiveMenuKind::SourceTree { .. }) {
            return;
        }
        let Some(row_index) = self.active_menu_entries().iter().position(|entry| {
            matches!(
                entry.action,
                MenuAction::OpenAssistantPicker { source_id: id } if id == source_id
            )
        }) else {
            return;
        };
        let anchor = active_menu.anchor
            + point(
                px(SOURCE_TREE_CONTEXT_MENU_WIDTH + 2.0),
                px(row_index as f32 * MENU_ROW_HEIGHT),
            );
        self.tab_menu_scroll = UniformListScrollHandle::new();
        self.active_menu = Some(ActiveMenu {
            kind: ActiveMenuKind::SourceTreeAssistantPicker { source_id },
            anchor,
        });
    }

    /// 把来源节点作为“@”引用添加到指定智能助手；`None` 表示新建助手页签。
    pub(crate) fn add_source_to_assistant(
        &mut self,
        source_id: SourceId,
        session_id: Option<usize>,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let Some(node) = self.source_registry.node(source_id) else {
            self.placeholder_notice = "未找到可添加的来源节点".to_string();
            return;
        };
        let Some(workspace_root) = self.source_workspace_root.as_ref() else {
            self.placeholder_notice = "日志工作目录已失效，请重新加载日志".to_string();
            return;
        };
        let SourceLocation::LocalPath(path) = &node.location;
        let Ok(relative) = path.strip_prefix(workspace_root) else {
            self.placeholder_notice = "该来源不在日志工作目录内，无法添加到智能助手".to_string();
            return;
        };
        let workspace_path = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if workspace_path.is_empty() {
            self.placeholder_notice = "无法把工作目录根添加到智能助手".to_string();
            return;
        }
        let is_folder = matches!(node.kind, SourceKind::Directory);
        let label = node.label.clone();

        let session_id = match session_id {
            Some(session_id) if self.assistant_tabs.contains_key(&session_id) => session_id,
            Some(_) => {
                self.placeholder_notice = "目标智能助手页签已不存在".to_string();
                return;
            }
            None => {
                self.open_assistant_tab(cx);
                let Some(session_id) = self.active_tab_id else {
                    return;
                };
                session_id
            }
        };
        if session_id != self.active_tab_id.unwrap_or_default() {
            self.activate_tab(session_id);
        }
        self.close_active_menu();
        let Some(assistant_tab) = self.assistant_tab(session_id) else {
            self.placeholder_notice = "目标智能助手页签已不存在".to_string();
            return;
        };
        assistant_tab.update(cx, |tab, tab_cx| {
            tab.add_workspace_mention(workspace_path, is_folder);
            tab_cx.notify();
        });
        if let Some(window) = window {
            let focus_handle = assistant_tab.read(cx).composer_focus_handle();
            // 切页或新建页签后，输入框在下一帧才完成挂载；延迟一帧聚焦可避免丢失键盘焦点。
            window.on_next_frame(move |window, _| focus_handle.focus(window));
        }
        self.placeholder_notice = format!("已将 {label} 添加到智能助手");
    }

    /// 返回指定助手会话实体；关闭页签后返回 `None`。
    pub(crate) fn assistant_tab(&self, session_id: usize) -> Option<Entity<AssistantTab>> {
        self.assistant_tabs.get(&session_id).cloned()
    }
}

/// 返回智能助手页签标题：第一个不带序号，后续依次追加 `2`、`3`……
fn assistant_tab_title(index: usize) -> String {
    if index <= 1 {
        "智能助手".to_string()
    } else {
        format!("智能助手 {index}")
    }
}
