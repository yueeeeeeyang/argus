//! 文件职责：声明正则缓存模块的后续扩展边界。
//! 创建日期：2026-06-09
//! 作者：Argus 开发团队
//! 主要功能：预留正则编译、容量限制和非法表达式提示入口。

/// 正则缓存占位结构，后续避免重复编译搜索表达式。
#[derive(Debug, Default)]
pub(crate) struct RegexCachePlaceholder;

impl RegexCachePlaceholder {
    /// 返回模块职责说明；当前不编译真实正则。
    pub(crate) fn responsibility(&self) -> &'static str {
        "缓存已编译正则并限制表达式复杂度；当前仅保留占位边界。"
    }
}
