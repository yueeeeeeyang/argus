//! 文件职责：声明搜索引擎的后续扩展边界。
//! 创建日期：2026-06-09
//! 作者：Argus 开发团队
//! 主要功能：预留流式搜索、结果上限、进度回报和取消能力。

/// 搜索引擎占位结构，后续协调搜索任务和读取器。
#[derive(Debug, Default)]
pub struct SearchEnginePlaceholder;

impl SearchEnginePlaceholder {
    /// 返回模块职责说明；当前不执行真实搜索。
    pub fn responsibility(&self) -> &'static str {
        "执行可取消的流式日志搜索；当前仅保留占位边界。"
    }
}
