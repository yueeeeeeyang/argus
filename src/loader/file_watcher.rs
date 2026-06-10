//! 文件职责：声明文件监听模块的后续扩展边界。
//! 创建日期：2026-06-09
//! 作者：Argus 开发团队
//! 主要功能：预留文件变更监听、热刷新和平台能力降级提示入口。

/// 文件监听占位结构，后续按平台封装文件系统事件。
#[derive(Debug, Default)]
pub struct FileWatcherPlaceholder;

impl FileWatcherPlaceholder {
    /// 返回模块职责说明；当前不注册真实文件系统监听。
    pub fn responsibility(&self) -> &'static str {
        "监听日志文件变化并触发刷新事件；当前仅保留占位边界。"
    }
}
