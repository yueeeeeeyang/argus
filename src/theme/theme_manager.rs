//! 文件职责：声明主题管理器的后续扩展边界。
//! 创建日期：2026-06-09
//! 作者：Argus 开发团队
//! 主要功能：预留主题加载、校验、切换和订阅通知的管理入口。

/// 主题管理器占位结构，后续负责加载内置主题和用户主题文件。
#[derive(Debug, Default)]
pub struct ThemeManagerPlaceholder;

impl ThemeManagerPlaceholder {
    /// 返回模块职责说明；当前不执行真实主题文件读取。
    pub fn responsibility(&self) -> &'static str {
        "管理主题加载、校验和切换；当前仅保留占位职责。"
    }
}
