//! 文件职责：声明日志读取器的后续扩展边界。
//! 创建日期：2026-06-09
//! 作者：Argus 开发团队
//! 主要功能：预留按来源 ID 打开日志、选择读取后端和生成首屏内容的入口。

/// 日志读取器占位结构，后续协调后端、分页和解码。
#[derive(Debug, Default)]
pub struct LogFileReaderPlaceholder;

impl LogFileReaderPlaceholder {
    /// 返回模块职责说明；当前不读取真实日志内容。
    pub fn responsibility(&self) -> &'static str {
        "协调日志读取、分页和解码；当前仅保留占位边界。"
    }
}
