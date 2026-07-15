//! 文件职责：远程连接领域模块入口，聚合 SSH/SMB 连接配置、SFTP 文件管理和 SSH 终端能力。
//! 创建日期：2026-07-08
//! 作者：Argus 开发团队
//! 主要功能：统一组织连接配置、SSH 公共能力、远程文件操作和终端会话。

pub(crate) mod connection;
pub(crate) mod sftp;
mod ssh;
pub(crate) mod terminal;
