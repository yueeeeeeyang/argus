//! 文件职责：导出 Argus 桌面界面层的所有视图与组件模块。
//! 创建日期：2026-06-09
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：组织主窗口、来源侧栏、内容视图、分析页面、设置窗口、远程会话和可复用组件。

pub(crate) mod archive_password_dialog;
pub(crate) mod components;
pub(crate) mod connection_dialog;
pub(crate) mod connection_tree_panel;
pub(crate) mod custom_title_bar;
pub(crate) mod dir_tree_panel;
pub(crate) mod file_preview_window;
pub(crate) mod highlight_colors;
pub(crate) mod input_native;
pub(crate) mod jstack_analysis_view;
pub(crate) mod jstack_thread_detail_window;
pub(crate) mod log_content_view;
pub(crate) mod log_search_window;
pub(crate) mod main_window;
pub(crate) mod remote_file_dialog;
pub(crate) mod remote_file_manager_view;
pub(crate) mod runtime_analysis_view;
pub(crate) mod settings_window;
pub(crate) mod source_panel;
pub(crate) mod source_picker;
pub(crate) mod source_resizer;
pub(crate) mod tab_bar;
pub(crate) mod terminal_view;
pub(crate) mod toolbar;
pub(crate) mod upgrade_dialog;
