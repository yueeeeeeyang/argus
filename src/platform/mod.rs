//! 文件职责：汇总 Argus 与操作系统集成相关的平台能力。
//! 创建日期：2026-06-15
//! 修改日期：2026-07-14
//! 作者：Argus 开发团队
//! 主要功能：对外暴露系统右键菜单、外部打开路径解析和自定义标题栏等跨平台入口。

pub(crate) mod custom_titlebar;
pub(crate) mod external_sources;
pub(crate) mod open_with_registration;
