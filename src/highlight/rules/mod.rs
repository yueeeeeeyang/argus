//! 文件职责：组织各语法类型的高亮规则模块。
//! 创建日期：2026-06-11
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：按格式拆分日志、配置文件和 Java 线程栈的高亮实现，保持模块边界清晰。

pub(crate) mod common;
pub(crate) mod java_thread;
pub(crate) mod json;
pub(crate) mod log;
pub(crate) mod properties;
pub(crate) mod xml;
pub(crate) mod yaml;
