//! 文件职责：定义 AI 分析会话状态、范围快照、资源预算、轨迹事件和追加消息。
//! 创建日期：2026-07-15
//! 修改日期：2026-07-16
//! 作者：Argus 开发团队
//! 主要功能：把来源树固化为不可变授权范围，并统一记录调用、Token、扫描量、原文量和取消边界。

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, atomic::AtomicUsize};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agent::report::DiagnosticReport;
use crate::config::{AiConfig, LoaderConfig, LogTypeProfile};
use crate::loader::archive::ArchivePasswordStore;
use crate::loader::{SourceId, SourceLocation, SourceRegistry};

/// 默认会话墙钟预算。
pub(crate) const MAX_SESSION_DURATION: Duration = Duration::from_secs(10 * 60);
/// 允许返回给模型的累计日志原文字节上限。
pub(crate) const MAX_RAW_LOG_BYTES: u64 = 512 * 1024;
/// 默认本地累计扫描量上限。
pub(crate) const MAX_LOCAL_SCAN_BYTES: u64 = 20 * 1024 * 1024 * 1024;
/// 单个工具 JSON 结果上限。
pub(crate) const MAX_TOOL_RESULT_BYTES: usize = 128 * 1024;
/// 单个工具结果中的日志原文上限。
pub(crate) const MAX_TOOL_RAW_BYTES: usize = 64 * 1024;

/// Agent 会话状态机中的稳定状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentSessionStatus {
    /// 会话已经创建，尚未开始模型调用。
    Created,
    /// 正在枚举来源和识别日志类型。
    Profiling,
    /// 正在执行模型与工具分析循环。
    Investigating,
    /// 正在校验并持久化最终报告。
    Reporting,
    /// 等待用户补充信息或授权。
    #[allow(dead_code)]
    AwaitingUser,
    /// 已完成分析并生成报告。
    Completed,
    /// 正在响应用户取消。
    Cancelling,
    /// 已取消且不再运行后台任务。
    Cancelled,
    /// 因配置、模型或工具错误结束。
    Failed,
}

impl AgentSessionStatus {
    /// 返回状态是否已经进入不可恢复终态。
    pub(crate) fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Failed)
    }

    /// 返回适合界面展示的中文状态。
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Created => "已创建",
            Self::Profiling => "识别日志",
            Self::Investigating => "分析中",
            Self::Reporting => "生成报告",
            Self::AwaitingUser => "等待用户",
            Self::Completed => "已完成",
            Self::Cancelling => "正在取消",
            Self::Cancelled => "已取消",
            Self::Failed => "失败",
        }
    }
}

/// Agent 轨迹条目类型。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AgentTraceKind {
    /// 会话状态变化。
    Status,
    /// 模型开始或完成一轮推理。
    Model,
    /// 模型流式返回的思考过程。
    Reasoning,
    /// 模型流式返回的可见正文。
    Output,
    /// 结构化工具开始或完成。
    Tool,
    /// 用户追加提示及消费回执。
    User,
    /// 非致命警告或终止错误。
    Warning,
    /// 最终结论摘要。
    Report,
}

/// 模型流式内容类型，用于让 UI 将同类增量合并到一条消息中。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AgentStreamKind {
    /// 模型思考过程；只在当前内存会话展示，不进入持久化报告。
    Reasoning,
    /// 模型面向用户的可见输出。
    Output,
}

/// 独立窗口展示的轻量轨迹条目，不包含完整工具原始输出。
#[derive(Clone, Debug)]
pub(crate) struct AgentTraceEntry {
    /// 条目生成时间。
    pub created_at: DateTime<Utc>,
    /// 条目类型。
    pub kind: AgentTraceKind,
    /// 一行标题。
    pub title: String,
    /// 已裁剪、可展示的详情。
    pub detail: String,
}

impl AgentTraceEntry {
    /// 创建不含日志原文的轨迹条目。
    pub(crate) fn new(
        kind: AgentTraceKind,
        title: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            created_at: Utc::now(),
            kind,
            title: title.into(),
            detail: detail.into(),
        }
    }
}

/// 追加提示在会话中的处理状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AgentUserMessageStatus {
    /// 已排队，等待下一模型边界注入。
    Queued,
    /// 已被编排器注入后续模型上下文。
    Consumed,
    /// 因状态或预算限制被拒绝。
    Rejected,
}

/// 独立窗口底部输入框提交的一条用户提示。
#[derive(Clone, Debug)]
pub(crate) struct AgentUserMessage {
    /// 随机消息标识。
    pub message_id: String,
    /// UTF-8 用户正文。
    pub content: String,
    /// 当前处理状态。
    pub status: AgentUserMessageStatus,
}

impl AgentUserMessage {
    /// 创建一条待消费提示。
    pub(crate) fn queued(content: String) -> Self {
        Self {
            message_id: Uuid::new_v4().to_string(),
            content,
            status: AgentUserMessageStatus::Queued,
        }
    }
}

/// 单个会话可访问的日志来源快照。
#[derive(Clone, Debug)]
pub(crate) struct SnapshotSource {
    /// 对模型暴露的不透明引用。
    pub source_ref: String,
    /// Argus 内部来源 ID，只供工具回到现有读取器。
    pub source_id: SourceId,
    /// 末级展示名，不包含真实父目录。
    pub file_name: String,
    /// 从分析根开始的相对展示路径。
    pub relative_path: String,
    /// 读取位置，绝不序列化给模型。
    pub location: SourceLocation,
    /// 已知文件或归档条目大小。
    pub size: Option<u64>,
    /// 名称规则选出的主日志配置 ID。
    pub profile_id: Option<String>,
}

/// 会话开始时固化的自定义日志说明。
#[derive(Clone, Debug)]
pub(crate) struct LogProfileSnapshot {
    /// 稳定配置 ID。
    pub profile_id: String,
    /// 用户可读类型名称。
    pub name: String,
    /// 分析说明正文。
    pub description: String,
    /// 说明内容摘要，供报告记录配置版本。
    pub description_sha256: String,
}

/// 当前 Agent 会话不可变来源范围和读取配置。
#[derive(Clone, Debug)]
pub(crate) struct SourceScopeSnapshot {
    /// 随机会话 ID。
    pub session_id: String,
    /// 来源根展示名称。
    pub root_label: String,
    /// 可作为日志打开的已加载叶子节点。
    pub sources: Arc<Vec<SnapshotSource>>,
    /// 按 ID 索引的日志说明快照。
    pub profiles: Arc<HashMap<String, LogProfileSnapshot>>,
    /// 当前默认日志编码。
    pub default_encoding: String,
    /// 现有 Jstack/Runtime 分析器使用的来源加载边界配置。
    pub loader_config: LoaderConfig,
    /// 当前进程内压缩包密码快照，只供底层读取器使用。
    pub archive_passwords: ArchivePasswordStore,
    /// 是否允许把工具返回的必要日志原文发送给模型。
    pub allow_raw_log_content: bool,
}

impl SourceScopeSnapshot {
    /// 从来源树的选中节点解析顶层根，并固化所有已加载日志候选。
    ///
    /// 参数说明：
    /// - `registry`：当前来源树。
    /// - `selected_id`：用户当前高亮节点；为空时仅允许来源树只有一个根。
    /// - `config`：已规范化 AI 配置和日志说明。
    /// - `default_encoding`：现有日志读取器使用的默认编码。
    /// - `archive_passwords`：仅存在进程内的压缩包密码快照。
    pub(crate) fn from_registry(
        registry: &SourceRegistry,
        selected_id: Option<SourceId>,
        config: &AiConfig,
        default_encoding: String,
        loader_config: LoaderConfig,
        archive_passwords: ArchivePasswordStore,
    ) -> Result<Self, String> {
        let root_id = match selected_id {
            Some(id) => registry
                .root_id_for(id)
                .ok_or_else(|| "当前选中来源不存在，无法确定 AI 分析范围".to_string())?,
            None if registry.root_ids().len() == 1 => registry.root_ids()[0],
            None if registry.root_ids().is_empty() => {
                return Err("请先加载日志来源".to_string());
            }
            None => return Err("存在多个来源根，请先在来源树中选择要分析的范围".to_string()),
        };
        let root = registry
            .node(root_id)
            .ok_or_else(|| "来源根已经失效".to_string())?;
        let mut profile_snapshots = build_profile_snapshots(&config.log_profiles);
        let mut sources = Vec::new();
        for source_id in registry.tree_order_source_ids() {
            let Some(node) = registry.node(*source_id) else {
                continue;
            };
            if !node.kind.is_log_candidate() || registry.root_id_for(*source_id) != Some(root_id) {
                continue;
            }
            let relative_path = relative_path_from_root(registry, root_id, *source_id);
            let profile_id = select_profile(&config.log_profiles, &node.label, &relative_path)
                .map(|profile| profile.profile_id.clone());
            sources.push(SnapshotSource {
                source_ref: Uuid::new_v4().to_string(),
                source_id: *source_id,
                file_name: node.label.clone(),
                relative_path,
                location: node.location.clone(),
                size: node.metadata.size,
                profile_id,
            });
        }
        if sources.is_empty() {
            return Err("当前来源根中没有已加载且可读取的日志文件".to_string());
        }
        // 会话只保留至少命中一个授权来源的说明，模型不能借工具枚举无关全局配置。
        let matched_profile_ids = sources
            .iter()
            .filter_map(|source| source.profile_id.as_deref())
            .collect::<BTreeSet<_>>();
        profile_snapshots.retain(|profile_id, _| matched_profile_ids.contains(profile_id.as_str()));
        Ok(Self {
            session_id: Uuid::new_v4().to_string(),
            root_label: root.label.clone(),
            sources: Arc::new(sources),
            profiles: Arc::new(profile_snapshots),
            default_encoding,
            loader_config,
            archive_passwords,
            allow_raw_log_content: config.allow_raw_log_content,
        })
    }

    /// 按不透明引用解析来源，未命中时拒绝而不是尝试解释成本地路径。
    pub(crate) fn source(&self, source_ref: &str) -> Option<&SnapshotSource> {
        self.sources
            .iter()
            .find(|source| source.source_ref == source_ref)
    }
}

/// 当前资源预算的只读快照，供轨迹窗口展示。
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct AgentBudgetSnapshot {
    /// 已发起模型请求数。
    pub model_requests: usize,
    /// 已执行工具调用数。
    pub tool_calls: usize,
    /// 模型累计输入 Token。
    pub input_tokens: u64,
    /// 模型累计输出 Token。
    pub output_tokens: u64,
    /// 服务端报告的累计总 Token；未报告时使用输入与输出之和。
    pub total_tokens: u64,
    /// 输出 Token 中用于模型内部思考的部分；仅在服务端提供时有值。
    pub reasoning_tokens: u64,
    /// 最近一次模型请求的输入 Token；服务端未返回 usage 时为空。
    pub latest_input_tokens: Option<u64>,
    /// 已扫描来源的保守核算字节数；工具完成后会纳入读取器报告的真实值。
    pub local_scan_bytes: u64,
    /// 已向模型返回日志原文字节数。
    pub raw_log_bytes: u64,
    /// 已运行墙钟秒数。
    pub elapsed_seconds: u64,
}

/// 线程安全预算计数器，模型 Hook 和工具共享同一实例。
pub(crate) struct AgentBudget {
    /// 会话开始时刻。
    started_at: Instant,
    /// 原子性要求不高但需成组校验的计数器。
    state: Mutex<AgentBudgetSnapshot>,
}

impl AgentBudget {
    /// 创建平衡档预算。
    pub(crate) fn balanced() -> Self {
        Self {
            started_at: Instant::now(),
            state: Mutex::new(AgentBudgetSnapshot::default()),
        }
    }

    /// 记录一次模型请求；调用次数不设产品上限，仍检查会话墙钟边界。
    pub(crate) fn record_model_request(&self) -> Result<AgentBudgetSnapshot, String> {
        self.ensure_time()?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| "AI 预算状态已损坏".to_string())?;
        state.model_requests = state.model_requests.saturating_add(1);
        state.elapsed_seconds = self.started_at.elapsed().as_secs();
        Ok(*state)
    }

    /// 记录一次工具调用及其预计本地扫描量；工具次数不设产品上限。
    pub(crate) fn record_tool_call(&self, scan_bytes: u64) -> Result<AgentBudgetSnapshot, String> {
        self.ensure_time()?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| "AI 预算状态已损坏".to_string())?;
        if state.local_scan_bytes.saturating_add(scan_bytes) > MAX_LOCAL_SCAN_BYTES {
            return Err("本地日志扫描量已达到 20 GiB 上限，需要用户明确扩容".to_string());
        }
        state.tool_calls = state.tool_calls.saturating_add(1);
        state.local_scan_bytes = state.local_scan_bytes.saturating_add(scan_bytes);
        state.elapsed_seconds = self.started_at.elapsed().as_secs();
        Ok(*state)
    }

    /// 用工具执行后获得的真实扫描量替换入口处预留量。
    ///
    /// 入口预留用于拒绝明显超限任务，执行后核算用于覆盖来源大小未知、压缩后膨胀等情况。
    /// 超限时仍保留真实累计值，使后续工具调用保持关闭而不会继续扩大读取范围。
    pub(crate) fn reconcile_tool_scan(
        &self,
        reserved_bytes: u64,
        actual_bytes: u64,
    ) -> Result<AgentBudgetSnapshot, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "AI 预算状态已损坏".to_string())?;
        state.local_scan_bytes = state
            .local_scan_bytes
            .saturating_sub(reserved_bytes)
            .saturating_add(actual_bytes);
        state.elapsed_seconds = self.started_at.elapsed().as_secs();
        if state.local_scan_bytes > MAX_LOCAL_SCAN_BYTES {
            return Err("本地日志扫描量已达到 20 GiB 上限，需要用户明确扩容".to_string());
        }
        Ok(*state)
    }

    /// 累加一轮模型返回的 Token 用量，并返回可直接展示的总量快照。
    pub(crate) fn record_token_usage(
        &self,
        input_tokens: u64,
        output_tokens: u64,
        total_tokens: u64,
        reasoning_tokens: u64,
    ) -> Result<AgentBudgetSnapshot, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "AI 预算状态已损坏".to_string())?;
        state.input_tokens = state.input_tokens.saturating_add(input_tokens);
        state.output_tokens = state.output_tokens.saturating_add(output_tokens);
        state.total_tokens = state.total_tokens.saturating_add(if total_tokens == 0 {
            input_tokens.saturating_add(output_tokens)
        } else {
            total_tokens
        });
        state.reasoning_tokens = state.reasoning_tokens.saturating_add(reasoning_tokens);
        state.latest_input_tokens = (input_tokens > 0).then_some(input_tokens);
        state.elapsed_seconds = self.started_at.elapsed().as_secs();
        Ok(*state)
    }

    /// 记录实际暴露给模型的日志原文字节数。
    pub(crate) fn consume_raw_log_bytes(
        &self,
        bytes: usize,
    ) -> Result<AgentBudgetSnapshot, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "AI 预算状态已损坏".to_string())?;
        if state.raw_log_bytes.saturating_add(bytes as u64) > MAX_RAW_LOG_BYTES {
            return Err("日志原文发送量已达到 512 KiB 上限".to_string());
        }
        state.raw_log_bytes = state.raw_log_bytes.saturating_add(bytes as u64);
        state.elapsed_seconds = self.started_at.elapsed().as_secs();
        Ok(*state)
    }

    /// 返回当前预算快照。
    pub(crate) fn snapshot(&self) -> AgentBudgetSnapshot {
        let mut snapshot = self.state.lock().map(|state| *state).unwrap_or_default();
        snapshot.elapsed_seconds = self.started_at.elapsed().as_secs();
        snapshot
    }

    /// 检查会话墙钟预算。
    fn ensure_time(&self) -> Result<(), String> {
        if self.started_at.elapsed() > MAX_SESSION_DURATION {
            Err("AI 分析已达到 10 分钟墙钟上限".to_string())
        } else {
            Ok(())
        }
    }
}

/// Agent 后台任务发往 GPUI 的增量事件。
#[derive(Clone, Debug)]
pub(crate) enum AgentEvent {
    /// 会话状态变化。
    Status(AgentSessionStatus),
    /// 新增轻量分析轨迹。
    Trace(AgentTraceEntry),
    /// 资源预算计数变化。
    Budget(AgentBudgetSnapshot),
    /// 模型思考或可见正文的流式增量；同类相邻事件由 UI 合并显示。
    StreamDelta(AgentStreamKind, String),
    /// 用户提示已被下一模型请求消费。
    UserMessageConsumed(String),
    /// 用户提示在最终报告或取消边界到达过晚，未进入模型上下文。
    UserMessageRejected(String, String),
    /// 最终结构化报告和可选持久化路径。
    Report(DiagnosticReport, Option<String>),
    /// 后台任务终止错误。
    Failed(String),
}

/// 工具和编排 Hook 共享的会话运行上下文。
pub(crate) struct AgentOperationContext {
    /// 不可变来源范围。
    pub scope: Arc<SourceScopeSnapshot>,
    /// 统一资源预算。
    pub budget: Arc<AgentBudget>,
    /// 取消令牌。
    pub cancellation: CancellationToken,
    /// 发送给 UI 的事件通道。
    pub event_sender: async_channel::Sender<AgentEvent>,
    /// 最终报告暂存槽。
    pub report: Mutex<Option<DiagnosticReport>>,
    /// 会话内大型工具结果制品；完整内容不进入轨迹，模型按 ID 分页读取。
    pub artifacts: Mutex<HashMap<String, String>>,
    /// 由搜索或上下文工具实际返回过的证据行范围；报告只能引用这些已观察范围。
    pub evidence_ranges: AgentEvidenceStore,
    /// 实际获取过分析说明的配置名称，最终报告会自动合并这些名称。
    pub used_log_profiles: Mutex<BTreeSet<String>>,
    /// 用户原始问题，提交报告时由可信会话层填充。
    pub question: String,
    /// 已排队但尚未注入模型上下文的用户提示数量。
    pub pending_user_messages: Arc<AtomicUsize>,
}

/// 会话内已经由确定性工具返回给模型的证据范围集合。
///
/// 范围使用 1 基闭区间保存，不持久化日志正文；报告提交时只允许引用某个已登记范围的子区间，
/// 从而阻止模型仅凭已知 `source_ref` 伪造不存在或从未读取的日志行。
#[derive(Debug, Default)]
pub(crate) struct AgentEvidenceStore {
    /// `(source_ref, start_line, end_line)` 的去重集合。
    ranges: Mutex<BTreeSet<(String, usize, usize)>>,
}

impl AgentEvidenceStore {
    /// 登记一次工具实际返回的有效 1 基闭区间。
    pub(crate) fn record(
        &self,
        source_ref: &str,
        start_line: usize,
        end_line: usize,
    ) -> Result<(), String> {
        if source_ref.is_empty() || start_line == 0 || end_line < start_line {
            return Err("AI 证据范围无效".to_string());
        }
        self.ranges
            .lock()
            .map_err(|_| "AI 证据登记状态已损坏".to_string())?
            .insert((source_ref.to_string(), start_line, end_line));
        Ok(())
    }

    /// 判断报告范围是否完全包含在某个已经返回给模型的证据范围内。
    pub(crate) fn contains(
        &self,
        source_ref: &str,
        start_line: usize,
        end_line: usize,
    ) -> Result<bool, String> {
        let ranges = self
            .ranges
            .lock()
            .map_err(|_| "AI 证据登记状态已损坏".to_string())?;
        Ok(ranges
            .iter()
            .any(|(recorded_ref, recorded_start, recorded_end)| {
                recorded_ref == source_ref
                    && *recorded_start <= start_line
                    && *recorded_end >= end_line
            }))
    }
}

impl AgentOperationContext {
    /// 发布轨迹，窗口关闭或接收端消失时静默丢弃，不能阻塞工具执行。
    pub(crate) fn trace(
        &self,
        kind: AgentTraceKind,
        title: impl Into<String>,
        detail: impl Into<String>,
    ) {
        let _ = self
            .event_sender
            .try_send(AgentEvent::Trace(AgentTraceEntry::new(kind, title, detail)));
    }

    /// 发布最新预算快照。
    pub(crate) fn publish_budget(&self) {
        let _ = self
            .event_sender
            .try_send(AgentEvent::Budget(self.budget.snapshot()));
    }

    /// 在每个工具入口执行取消、扫描量校验和无上限调用计数。
    pub(crate) fn begin_tool(&self, tool_name: &str, scan_bytes: u64) -> Result<(), String> {
        if self.cancellation.is_cancelled() {
            return Err("会话已取消".to_string());
        }
        let budget = self.budget.record_tool_call(scan_bytes)?;
        let _ = self.event_sender.try_send(AgentEvent::Budget(budget));
        self.trace(
            AgentTraceKind::Tool,
            format!("调用 {tool_name}"),
            "参数已通过范围与预算校验",
        );
        Ok(())
    }
}

/// 按配置顺序和优先级选择一个主日志配置。
fn select_profile<'a>(
    profiles: &'a [LogTypeProfile],
    file_name: &str,
    relative_path: &str,
) -> Option<&'a LogTypeProfile> {
    profiles
        .iter()
        .enumerate()
        .filter(|(_, profile)| {
            // 会话入口已经整体校验配置；这里位于逐来源热路径，不能为每个文件重复执行规则校验。
            profile.enabled
                && profile
                    .matchers
                    .iter()
                    .any(|matcher| matcher.is_match(file_name, relative_path))
        })
        .max_by(|(left_index, left), (right_index, right)| {
            left.priority
                .cmp(&right.priority)
                .then_with(|| right_index.cmp(left_index))
        })
        .map(|(_, profile)| profile)
}

/// 固化所有有效且启用的日志说明及摘要。
fn build_profile_snapshots(profiles: &[LogTypeProfile]) -> HashMap<String, LogProfileSnapshot> {
    profiles
        .iter()
        .filter(|profile| profile.enabled && profile.validate().is_ok())
        .map(|profile| {
            let snapshot = LogProfileSnapshot {
                profile_id: profile.profile_id.clone(),
                name: profile.name.clone(),
                description: profile.description.clone(),
                description_sha256: hex::encode(Sha256::digest(profile.description.as_bytes())),
            };
            (snapshot.profile_id.clone(), snapshot)
        })
        .collect()
}

/// 构造来源根内的正斜杠相对展示路径。
fn relative_path_from_root(
    registry: &SourceRegistry,
    root_id: SourceId,
    source_id: SourceId,
) -> String {
    let mut labels = Vec::new();
    let mut current_id = Some(source_id);
    while let Some(id) = current_id {
        let Some(node) = registry.node(id) else {
            break;
        };
        if id != root_id {
            labels.push(node.label.clone());
        }
        if id == root_id {
            break;
        }
        current_id = node.parent_id;
    }
    labels.reverse();
    labels.join("/")
}

/// 创建供 UI 和后台任务共同持有的取消令牌。
pub(crate) fn new_cancellation_token() -> CancellationToken {
    CancellationToken::new()
}

/// 按 UTF-8 字符边界把文本裁剪到指定字节数，并在发生裁剪时追加省略号。
///
/// `String::truncate` 要求索引正好位于字符边界；模型、日志和服务端错误都可能包含多字节文本，
/// 因此统一向前寻找合法边界，避免错误处理路径反而触发 panic。
pub(crate) fn truncate_utf8_with_ellipsis(mut value: String, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value;
    }
    let mut boundary = max_bytes.min(value.len());
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
    value.push('…');
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LogNameMatcher, LogNameMatcherMode, LogNameMatcherTarget};
    use crate::loader::{SourceKind, SourceMetadata, SourceTreeNode};
    use std::path::PathBuf;

    /// 构造会匹配 `app.log` 的测试日志配置。
    fn test_profile(name: &str, priority: u16, enabled: bool) -> LogTypeProfile {
        LogTypeProfile {
            profile_id: Uuid::new_v4().to_string(),
            enabled,
            name: name.to_string(),
            priority,
            matchers: vec![LogNameMatcher {
                target: LogNameMatcherTarget::FileName,
                mode: LogNameMatcherMode::Exact,
                pattern: "app.log".to_string(),
                case_sensitive: false,
            }],
            description: "测试日志说明".to_string(),
        }
    }

    /// 验证优先级更高的日志说明胜出，禁用配置不参与匹配。
    #[test]
    fn profile_selection_respects_enabled_and_priority() {
        let profiles = vec![
            test_profile("禁用高优先级", 1000, false),
            test_profile("低优先级", 100, true),
            test_profile("高优先级", 200, true),
        ];
        let selected =
            select_profile(&profiles, "APP.LOG", "logs/APP.LOG").expect("应匹配日志配置");
        assert_eq!(selected.name, "高优先级");
    }

    /// 验证相同优先级时设置列表中更靠前的配置胜出。
    #[test]
    fn profile_selection_uses_configuration_order_as_tiebreaker() {
        let profiles = vec![
            test_profile("第一项", 100, true),
            test_profile("第二项", 100, true),
        ];
        let selected = select_profile(&profiles, "app.log", "app.log").expect("应匹配日志配置");
        assert_eq!(selected.name, "第一项");
    }

    /// 验证模型与工具调用只累计次数，不再因次数终止；原文与扫描安全边界仍生效。
    #[test]
    fn balanced_budget_keeps_data_limits_without_call_count_limits() {
        let budget = AgentBudget::balanced();
        for _ in 0..128 {
            budget
                .record_model_request()
                .expect("模型调用计数不应形成次数上限");
        }
        for _ in 0..256 {
            budget
                .record_tool_call(0)
                .expect("工具调用计数不应形成次数上限");
        }
        let snapshot = budget.snapshot();
        assert_eq!(snapshot.model_requests, 128);
        assert_eq!(snapshot.tool_calls, 256);

        let raw_budget = AgentBudget::balanced();
        raw_budget
            .consume_raw_log_bytes(MAX_RAW_LOG_BYTES as usize)
            .expect("预算内原文应成功");
        assert!(raw_budget.consume_raw_log_bytes(1).is_err());
        assert!(budget.record_tool_call(MAX_LOCAL_SCAN_BYTES + 1).is_err());
    }

    /// 验证工具完成后的真实扫描量会替换入口预留量，并在膨胀后执行同一上限。
    #[test]
    fn tool_scan_reconciliation_uses_actual_bytes() {
        let budget = AgentBudget::balanced();
        budget.record_tool_call(64).expect("入口预留应成功");
        let snapshot = budget
            .reconcile_tool_scan(64, 128)
            .expect("实际扫描量仍在预算内");
        assert_eq!(snapshot.local_scan_bytes, 128);

        let exceeded = AgentBudget::balanced();
        exceeded.record_tool_call(1).expect("入口预留应成功");
        assert!(
            exceeded
                .reconcile_tool_scan(1, MAX_LOCAL_SCAN_BYTES + 1)
                .is_err()
        );
        assert_eq!(
            exceeded.snapshot().local_scan_bytes,
            MAX_LOCAL_SCAN_BYTES + 1
        );
    }

    /// 验证报告只能引用工具已经返回的同来源行号子区间。
    #[test]
    fn evidence_store_rejects_unobserved_ranges() {
        let evidence = AgentEvidenceStore::default();
        evidence
            .record("opaque-source", 10, 20)
            .expect("有效证据应登记成功");
        assert!(evidence.contains("opaque-source", 12, 18).unwrap());
        assert!(!evidence.contains("opaque-source", 9, 18).unwrap());
        assert!(!evidence.contains("opaque-source", 12, 21).unwrap());
        assert!(!evidence.contains("another-source", 12, 18).unwrap());
    }

    /// 验证各轮 Token 用量累加，并在服务端缺少 total 时使用输入输出之和。
    #[test]
    fn token_usage_accumulates_with_total_fallback() {
        let budget = AgentBudget::balanced();
        budget
            .record_token_usage(100, 20, 120, 8)
            .expect("首轮 Token 应记录成功");
        let snapshot = budget
            .record_token_usage(40, 10, 0, 4)
            .expect("缺少总量时应使用输入输出之和");
        assert_eq!(snapshot.input_tokens, 140);
        assert_eq!(snapshot.output_tokens, 30);
        assert_eq!(snapshot.total_tokens, 170);
        assert_eq!(snapshot.reasoning_tokens, 12);
        assert_eq!(snapshot.latest_input_tokens, Some(40));
        let unavailable_snapshot = budget
            .record_token_usage(0, 0, 0, 0)
            .expect("缺少 usage 的轮次仍应被安全记录");
        assert_eq!(unavailable_snapshot.latest_input_tokens, None);
    }

    /// 验证长中文错误按合法 UTF-8 边界裁剪，不会在异常处理路径触发 panic。
    #[test]
    fn utf8_truncation_preserves_character_boundaries() {
        let value = "错误".repeat(800);
        let truncated = truncate_utf8_with_ellipsis(value, 1024);
        assert!(truncated.ends_with('…'));
        assert!(truncated.len() <= 1027);
        assert!(std::str::from_utf8(truncated.as_bytes()).is_ok());
    }

    /// 验证来源快照只保留命中过当前范围的日志说明，且模型引用不暴露真实路径。
    #[test]
    fn source_snapshot_filters_unmatched_guidance_and_uses_opaque_references() {
        let mut registry = SourceRegistry::new();
        let root_id = registry.allocate_id();
        registry.insert_node(SourceTreeNode {
            id: root_id,
            parent_id: None,
            depth: 0,
            label: "logs".to_string(),
            kind: SourceKind::Directory,
            location: SourceLocation::LocalPath(PathBuf::from("/private/company/logs")),
            metadata: SourceMetadata {
                children_loaded: true,
                ..SourceMetadata::default()
            },
            selected: false,
            expanded: true,
        });
        let source_id = registry.allocate_id();
        registry.insert_node(SourceTreeNode {
            id: source_id,
            parent_id: Some(root_id),
            depth: 1,
            label: "app.log".to_string(),
            kind: SourceKind::LogFile,
            location: SourceLocation::LocalPath(PathBuf::from("/private/company/logs/app.log")),
            metadata: SourceMetadata {
                size: Some(128),
                children_loaded: true,
                ..SourceMetadata::default()
            },
            selected: false,
            expanded: false,
        });
        registry.rebuild_all_indices();
        let matched = test_profile("应用日志", 100, true);
        let unmatched = LogTypeProfile {
            profile_id: Uuid::new_v4().to_string(),
            enabled: true,
            name: "审计日志".to_string(),
            priority: 100,
            matchers: vec![LogNameMatcher {
                target: LogNameMatcherTarget::FileName,
                mode: LogNameMatcherMode::Exact,
                pattern: "audit.log".to_string(),
                case_sensitive: false,
            }],
            description: "审计说明".to_string(),
        };
        let config = AiConfig {
            log_profiles: vec![matched.clone(), unmatched],
            ..AiConfig::default()
        };
        let snapshot = SourceScopeSnapshot::from_registry(
            &registry,
            None,
            &config,
            "UTF-8".to_string(),
            LoaderConfig::default(),
            ArchivePasswordStore::default(),
        )
        .expect("应创建来源快照");
        assert_eq!(snapshot.sources.len(), 1);
        assert_eq!(snapshot.sources[0].relative_path, "app.log");
        assert_eq!(
            snapshot.sources[0].profile_id.as_deref(),
            Some(matched.profile_id.as_str())
        );
        assert_eq!(snapshot.profiles.len(), 1);
        assert!(!snapshot.sources[0].source_ref.contains("private"));
        assert!(Uuid::parse_str(&snapshot.sources[0].source_ref).is_ok());
    }
}
