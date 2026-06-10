//! 文件职责：声明行号索引模块的后续扩展边界。
//! 创建日期：2026-06-09
//! 作者：Argus 开发团队
//! 主要功能：预留行起始字节、行长度和后台索引进度管理入口。

/// 行号索引占位结构，后续支持跳行和搜索结果定位。
#[derive(Debug, Default)]
pub struct LineIndexPlaceholder;

impl LineIndexPlaceholder {
    /// 返回模块职责说明；当前不扫描真实日志行。
    pub fn responsibility(&self) -> &'static str {
        "建立行号与字节范围映射；当前仅保留占位边界。"
    }
}
