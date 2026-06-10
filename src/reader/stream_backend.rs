//! 文件职责：声明顺序流后端的后续扩展边界。
//! 创建日期：2026-06-09
//! 作者：Argus 开发团队
//! 主要功能：预留压缩包条目、管道或远程来源的顺序预览读取能力。

/// 顺序流后端占位结构，后续处理不可随机访问来源。
#[derive(Debug, Default)]
pub struct StreamBackendPlaceholder;

impl StreamBackendPlaceholder {
    /// 返回模块职责说明；当前不打开真实流。
    pub fn responsibility(&self) -> &'static str {
        "提供顺序预览读取能力；当前仅保留占位边界。"
    }
}
