//! 文件职责：组织 Argus AI 日志分析 Agent 的领域模块和公共入口。
//! 创建日期：2026-07-15
//! 修改日期：2026-07-16
//! 作者：Argus 开发团队
//! 主要功能：导出凭据、会话编排、报告、来源快照和结构化工具所需类型。

pub(crate) mod advanced_tools;
pub(crate) mod credential;
pub(crate) mod model_gateway;
pub(crate) mod orchestrator;
pub(crate) mod report;
pub(crate) mod runtime;
pub(crate) mod session;
pub(crate) mod source_scan;
pub(crate) mod tools;

pub(crate) use credential::{load_api_key, save_api_key};
pub(crate) use model_gateway::probe_model_capabilities;
pub(crate) use orchestrator::{AgentRunRequest, run_agent_session};
pub(crate) use report::{DiagnosticFinding, DiagnosticReport};
pub(crate) use runtime::agent_runtime;
pub(crate) use session::{
    AgentAnalysisStage, AgentAnalysisStageEvent, AgentAnalysisStageStatus, AgentBudgetSnapshot,
    AgentEvent, AgentSessionStatus, AgentStreamKind, AgentTraceEntry, AgentTraceKind,
    AgentUserMessage, AgentUserMessageStatus, SourceScopeSnapshot,
};
pub(crate) use source_scan::{
    AgentLogProfileMatchSummary, AgentSourcePreparation, prepare_agent_source_scope,
};
