//! 文件职责：组织 Argus AI 日志分析 Agent 的领域模块和公共入口。
//! 创建日期：2026-07-15
//! 修改日期：2026-07-17
//! 作者：Argus 开发团队
//! 主要功能：导出凭据、会话编排、Agent 专用日志访问、报告、来源快照和结构化工具所需类型。

pub(crate) mod advanced_tools;
pub(crate) mod analyzers;
pub(crate) mod assistant;
pub(crate) mod batch_search;
pub(crate) mod credential;
pub(crate) mod log_access;
pub(crate) mod log_search;
pub(crate) mod model_gateway;
pub(crate) mod orchestrator;
pub(crate) mod report;
pub(crate) mod runtime;
pub(crate) mod session;
pub(crate) mod source_scan;
pub(crate) mod tools;

pub(crate) use assistant::{AssistantHistoryTurn, AssistantRunRequest, run_assistant_turn};
pub(crate) use credential::{load_api_key, save_api_key};
pub(crate) use model_gateway::probe_model_capabilities;
pub(crate) use orchestrator::{AgentRunRequest, run_agent_session};
pub(crate) use report::{DiagnosticFinding, DiagnosticReport};
pub(crate) use runtime::agent_runtime;
pub(crate) use session::{
    AgentAnalysisStageEvent, AgentAnalysisStageStatus, AgentBudgetSnapshot, AgentEvent,
    AgentScopeSelection, AgentSessionStatus, AgentStreamKind, AgentTraceEntry, AgentTraceKind,
    AgentUserMessage, AgentUserMessageStatus, SourceScopeSnapshot,
};
pub(crate) use source_scan::{
    AgentLogProfileMatchSummary, AgentSourcePreparation, prepare_agent_source_scope,
    prepare_agent_source_scope_for_selection,
};

#[cfg(test)]
mod architecture_tests {
    use std::fs;
    use std::path::Path;

    /// Agent 模型工具允许依赖的实现文件；这些文件共同构成独立的工具执行边界。
    const AGENT_TOOL_IMPLEMENTATION_FILES: &[&str] = &[
        "src/agent/tools.rs",
        "src/agent/advanced_tools.rs",
        "src/agent/batch_search.rs",
        "src/agent/log_access.rs",
        "src/agent/log_search.rs",
        "src/agent/analyzers.rs",
        "src/agent/source_scan.rs",
    ];
    /// 主窗口业务模块的导入前缀；模型工具不得重新接回这些展示或交互流程。
    const FORBIDDEN_BUSINESS_IMPORTS: &[&str] =
        &["crate::reader::", "crate::search::", "crate::analysis::"];

    /// 防止后续功能扩展把 Agent 工具重新绑定到主窗口阅读、搜索或分析页面实现。
    #[test]
    fn model_tools_do_not_import_main_window_business_modules() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        for relative_path in AGENT_TOOL_IMPLEMENTATION_FILES {
            let source = fs::read_to_string(manifest_dir.join(relative_path))
                .unwrap_or_else(|error| panic!("无法读取 Agent 工具实现 {relative_path}：{error}"));
            for forbidden_import in FORBIDDEN_BUSINESS_IMPORTS {
                assert!(
                    !source.contains(forbidden_import),
                    "Agent 工具实现 {relative_path} 禁止导入主窗口业务模块 {forbidden_import}"
                );
            }
            if relative_path.ends_with("batch_search.rs") {
                assert!(
                    !source.contains("crate::agent::log_access"),
                    "Agent 流式批量搜索不得重新接入全文日志文档或行索引"
                );
            }
        }
    }
}
