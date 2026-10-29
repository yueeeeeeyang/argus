//! 文件职责：远程连接领域模块入口，聚合连接配置、通用远程文件管理和 SSH 终端能力。
//! 创建日期：2026-07-08
//! 作者：Argus 开发团队
//! 修改日期：2026-10-15
//! 主要功能：统一组织连接配置、SSH 公共能力、Git/SVN 仓库浏览、远程文件操作和终端会话。

pub(crate) mod connection;
mod download;
pub(crate) mod git;
pub(crate) mod remote_file;
mod ssh;
mod svn;
pub(crate) mod svn_certificate;
mod svn_http;
pub(crate) mod svn_tls;
pub(crate) mod terminal;
pub(crate) mod terminal_input;
mod worker;
