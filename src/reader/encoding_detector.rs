//! 文件职责：声明编码检测模块的后续扩展边界。
//! 创建日期：2026-06-09
//! 作者：Argus 开发团队
//! 主要功能：预留 BOM、UTF-8 校验、启发式检测和用户覆盖入口。

/// 编码检测器占位结构，后续负责样本检测和置信度输出。
#[derive(Debug, Default)]
pub struct EncodingDetectorPlaceholder;

impl EncodingDetectorPlaceholder {
    /// 返回模块职责说明；当前不读取真实字节样本。
    pub fn responsibility(&self) -> &'static str {
        "检测日志编码并支持用户覆盖；当前仅保留占位边界。"
    }
}
