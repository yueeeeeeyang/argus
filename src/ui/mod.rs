//! 文件职责：导出 Argus 桌面界面层的所有视图与组件模块。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-16
//! 作者：Argus 开发团队
//! 主要功能：组织主窗口、自定义标题栏、活动栏、来源侧栏、内容区、Jstack 分析页、升级弹窗和可复用组件。

pub mod activity_bar;
pub mod components;
pub mod connection_dialog;
pub mod connection_tree_panel;
pub mod custom_title_bar;
pub mod dir_tree_panel;
pub mod file_preview_window;
pub mod input_native;
pub mod jstack_analysis_view;
pub mod jstack_thread_detail_window;
pub mod log_content_view;
pub mod log_search_window;
pub mod main_window;
pub mod placeholder_dialog;
pub mod runtime_analysis_view;
pub mod settings_page;
pub mod settings_window;
pub mod sftp_dialog;
pub mod sftp_file_manager_view;
pub mod source_panel;
pub mod source_picker;
pub mod source_resizer;
pub mod status_bar;
pub mod tab_bar;
pub mod terminal_view;
pub mod toolbar;
pub mod upgrade_dialog;
