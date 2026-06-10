//! 文件职责：声明过滤器模块的后续扩展边界。
//! 创建日期：2026-06-09
//! 作者：Argus 开发团队
//! 主要功能：预留按级别、关键字和结构化字段过滤日志的入口。

/// 过滤器占位结构，后续对搜索结果或日志行进行条件筛选。
#[derive(Debug, Default)]
pub struct FilterPlaceholder;

impl FilterPlaceholder {
    /// 返回模块职责说明；当前不执行真实过滤。
    pub fn responsibility(&self) -> &'static str {
        "按用户条件过滤日志行和搜索结果；当前仅保留占位边界。"
    }
}
