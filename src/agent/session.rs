//! 文件职责：定义 AI 分析会话状态、范围快照、资源预算、轨迹事件和追加消息。
//! 创建日期：2026-07-15
//! 修改日期：2026-07-16
//! 作者：Argus 开发团队
//! 主要功能：把来源树固化为不可变授权范围，并统一记录调用、Token、扫描量、独立复核和取消边界。

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize},
};
use std::time::Instant;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agent::report::{DiagnosticReport, EvidenceDisplayExcerpt};
use crate::config::{AiConfig, LoaderConfig, LogNameMatcher, LogTypeProfile};
use crate::loader::archive::ArchivePasswordStore;
use crate::loader::{SourceId, SourceLocation, SourceRegistry};
use crate::reader::log_file_reader::LogReaderHandle;

/// 单个工具 JSON 结果上限。
pub(crate) const MAX_TOOL_RESULT_BYTES: usize = 128 * 1024;
/// 单个工具结果中的日志原文上限。
pub(crate) const MAX_TOOL_RAW_BYTES: usize = 64 * 1024;
/// 会话内最多缓存的日志读取器数量，限制解码正文、行索引和归档落盘文件占用。
const MAX_AGENT_READER_CACHE_ENTRIES: usize = 2;
/// 会话内最多缓存的事件签名统计数量，防止模型构造高基数签名耗尽内存。
const MAX_EVENT_OCCURRENCE_CACHE_ENTRIES: usize = 256;

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

/// 固定日志分析流程中的十二个可展示阶段。
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentAnalysisStage {
    /// A：完整扫描来源树。
    ScanSources,
    /// B：匹配日志类型和结构化说明。
    MatchLogTypes,
    /// C：拆解用户问题。
    BreakDownQuestion,
    /// D：建立分析计划和覆盖清单。
    BuildPlan,
    /// E：分层采样与异常检索。
    SearchAnomalies,
    /// F：提取事件上下文。
    ExtractContext,
    /// G：构建跨来源时间线。
    BuildTimeline,
    /// H：形成候选假设。
    FormHypotheses,
    /// I：搜索支持证据和反证。
    VerifyHypotheses,
    /// J：本地验证引用。
    ValidateEvidence,
    /// K：独立复核结论。
    IndependentReview,
    /// L：生成三段式报告。
    GenerateReport,
}

impl AgentAnalysisStage {
    /// 固定阶段顺序，供状态跟踪器和右侧悬浮时间线共享。
    pub(crate) const ALL: [Self; 12] = [
        Self::ScanSources,
        Self::MatchLogTypes,
        Self::BreakDownQuestion,
        Self::BuildPlan,
        Self::SearchAnomalies,
        Self::ExtractContext,
        Self::BuildTimeline,
        Self::FormHypotheses,
        Self::VerifyHypotheses,
        Self::ValidateEvidence,
        Self::IndependentReview,
        Self::GenerateReport,
    ];

    /// 返回阶段在固定流程中的零基位置。
    pub(crate) fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|stage| *stage == self)
            .expect("固定分析阶段必须存在于 ALL 中")
    }

    /// 返回阶段时间线使用的简洁中文标题。
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::ScanSources => "完整扫描来源树",
            Self::MatchLogTypes => "匹配日志类型与说明",
            Self::BreakDownQuestion => "拆解用户问题",
            Self::BuildPlan => "建立计划与覆盖清单",
            Self::SearchAnomalies => "分层采样与异常检索",
            Self::ExtractContext => "提取事件上下文",
            Self::BuildTimeline => "构建跨来源时间线",
            Self::FormHypotheses => "形成候选假设",
            Self::VerifyHypotheses => "搜索支持证据与反证",
            Self::ValidateEvidence => "本地验证引用",
            Self::IndependentReview => "独立复核结论",
            Self::GenerateReport => "生成三段式报告",
        }
    }

    /// 返回没有模型摘要时使用的保守阶段结果，保证完成节点始终存在结果说明。
    pub(crate) fn default_result_summary(self) -> &'static str {
        match self {
            Self::ScanSources => "来源树扫描已完成",
            Self::MatchLogTypes => "日志类型与说明匹配已完成",
            Self::BreakDownQuestion => "用户问题已完成结构化拆解",
            Self::BuildPlan => "分析计划与覆盖清单已建立",
            Self::SearchAnomalies => "分层采样与异常检索已完成",
            Self::ExtractContext => "候选事件上下文已提取",
            Self::BuildTimeline => "跨来源时间线已构建",
            Self::FormHypotheses => "候选假设已形成",
            Self::VerifyHypotheses => "支持证据与反证检索已完成",
            Self::ValidateEvidence => "报告引用已通过本地验证",
            Self::IndependentReview => "结论已完成独立复核",
            Self::GenerateReport => "三段式报告已生成",
        }
    }
}

/// 单个分析阶段的最终结果或当前运行状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AgentAnalysisStageStatus {
    /// 尚未进入该阶段。
    Pending,
    /// 当前正在执行。
    Running,
    /// 已正常完成。
    Completed,
    /// 会话在该阶段因不可恢复错误失败。
    Failed,
    /// 用户在该阶段主动取消。
    Cancelled,
}

/// 后台发送给阶段时间线卡片的结构化状态快照。
#[derive(Clone, Debug)]
pub(crate) struct AgentAnalysisStageEvent {
    /// 对应固定阶段。
    pub stage: AgentAnalysisStage,
    /// 当前结果状态。
    pub status: AgentAnalysisStageStatus,
    /// 阶段已消耗秒数；运行态由界面在此基础上继续计时。
    pub elapsed_seconds: u64,
    /// 完成、失败或取消阶段的简短结果摘要；不包含具体思考、工具参数或日志原文。
    pub result_summary: Option<String>,
}

/// 十二阶段的单会话顺序跟踪器，只允许向后推进，避免模型让时间线状态倒退。
pub(crate) struct AgentAnalysisStageTracker {
    /// 每个阶段的当前状态。
    statuses: [AgentAnalysisStageStatus; 12],
    /// 已完成阶段的最终耗时。
    elapsed_seconds: [u64; 12],
    /// 每个完成阶段的简短结果摘要。
    result_summaries: [Option<String>; 12],
    /// 当前运行阶段开始时间。
    current_started_at: Instant,
}

impl AgentAnalysisStageTracker {
    /// 使用启动前已经测得的来源扫描、类型匹配结果和耗时创建跟踪器，并从问题拆解阶段开始。
    pub(crate) fn new(
        source_scan_seconds: u64,
        profile_seconds: u64,
        source_scan_summary: String,
        profile_summary: String,
    ) -> Self {
        let mut statuses = [AgentAnalysisStageStatus::Pending; 12];
        let mut elapsed_seconds = [0_u64; 12];
        let mut result_summaries = std::array::from_fn(|_| None);
        statuses[AgentAnalysisStage::ScanSources.index()] = AgentAnalysisStageStatus::Completed;
        statuses[AgentAnalysisStage::MatchLogTypes.index()] = AgentAnalysisStageStatus::Completed;
        statuses[AgentAnalysisStage::BreakDownQuestion.index()] = AgentAnalysisStageStatus::Running;
        elapsed_seconds[AgentAnalysisStage::ScanSources.index()] = source_scan_seconds;
        elapsed_seconds[AgentAnalysisStage::MatchLogTypes.index()] = profile_seconds;
        result_summaries[AgentAnalysisStage::ScanSources.index()] = Some(
            normalize_stage_result_summary(source_scan_summary, AgentAnalysisStage::ScanSources),
        );
        result_summaries[AgentAnalysisStage::MatchLogTypes.index()] = Some(
            normalize_stage_result_summary(profile_summary, AgentAnalysisStage::MatchLogTypes),
        );
        Self {
            statuses,
            elapsed_seconds,
            result_summaries,
            current_started_at: Instant::now(),
        }
    }

    /// 返回全部阶段的当前快照。
    pub(crate) fn snapshots(&self) -> Vec<AgentAnalysisStageEvent> {
        AgentAnalysisStage::ALL
            .iter()
            .copied()
            .map(|stage| self.event(stage))
            .collect()
    }

    /// 按固定流程推进到紧邻的下一阶段，并返回发生变化的阶段事件。
    ///
    /// 重复或倒序请求仍按幂等操作忽略；跨阶段请求会显式报错，避免尚未执行的阶段
    /// 被错误标记为完成并在时间线中产生虚假的结果摘要。
    pub(crate) fn advance(
        &mut self,
        target: AgentAnalysisStage,
        completed_summary: Option<String>,
    ) -> Result<Vec<AgentAnalysisStageEvent>, String> {
        let target_index = target.index();
        let current_index = self
            .statuses
            .iter()
            .position(|status| *status == AgentAnalysisStageStatus::Running)
            .unwrap_or(target_index);
        if target_index <= current_index {
            return Ok(Vec::new());
        }
        if target_index != current_index + 1 {
            let current_stage = AgentAnalysisStage::ALL[current_index];
            let next_stage = AgentAnalysisStage::ALL[current_index + 1];
            return Err(format!(
                "分析阶段必须按固定顺序推进：当前为“{}”，下一阶段只能是“{}”，不能直接进入“{}”",
                current_stage.title(),
                next_stage.title(),
                target.title()
            ));
        }

        let mut events = Vec::new();
        let current_stage = AgentAnalysisStage::ALL[current_index];
        self.statuses[current_index] = AgentAnalysisStageStatus::Completed;
        self.elapsed_seconds[current_index] = self.current_started_at.elapsed().as_secs();
        self.result_summaries[current_index] = Some(normalize_stage_result_summary(
            completed_summary.unwrap_or_default(),
            current_stage,
        ));
        events.push(self.event(current_stage));
        self.statuses[target_index] = AgentAnalysisStageStatus::Running;
        self.elapsed_seconds[target_index] = 0;
        self.result_summaries[target_index] = None;
        self.current_started_at = Instant::now();
        events.push(self.event(target));
        Ok(events)
    }

    /// 完成当前阶段；最终报告成功后用于关闭最后一个加载动画。
    pub(crate) fn complete_current(
        &mut self,
        completed_summary: Option<String>,
    ) -> Vec<AgentAnalysisStageEvent> {
        let Some(current_index) = self
            .statuses
            .iter()
            .position(|status| *status == AgentAnalysisStageStatus::Running)
        else {
            return Vec::new();
        };
        let current_stage = AgentAnalysisStage::ALL[current_index];
        self.statuses[current_index] = AgentAnalysisStageStatus::Completed;
        self.elapsed_seconds[current_index] = self.current_started_at.elapsed().as_secs();
        self.result_summaries[current_index] = Some(normalize_stage_result_summary(
            completed_summary.unwrap_or_default(),
            current_stage,
        ));
        vec![self.event(current_stage)]
    }

    /// 构造单个阶段事件；运行态耗时包含当前已经经过的时间。
    fn event(&self, stage: AgentAnalysisStage) -> AgentAnalysisStageEvent {
        let index = stage.index();
        let elapsed_seconds = if self.statuses[index] == AgentAnalysisStageStatus::Running {
            self.current_started_at.elapsed().as_secs()
        } else {
            self.elapsed_seconds[index]
        };
        AgentAnalysisStageEvent {
            stage,
            status: self.statuses[index],
            elapsed_seconds,
            result_summary: self.result_summaries[index].clone(),
        }
    }
}

/// 规范阶段结果摘要并限制展示长度，避免模型内容破坏时间线布局或复制大段日志。
fn normalize_stage_result_summary(summary: String, stage: AgentAnalysisStage) -> String {
    let compact = summary.split_whitespace().collect::<Vec<_>>().join(" ");
    let value = if compact.is_empty() {
        stage.default_result_summary().to_string()
    } else {
        compact
    };
    truncate_utf8_with_ellipsis(value, 240)
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
    /// 名称规则优先级，来源概览按该值解释重叠匹配。
    pub priority: u16,
    /// 会话创建时固化的名称规则，供元数据概览统计复用。
    pub matchers: Vec<LogNameMatcher>,
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

    /// 记录一次模型请求；调用次数和会话运行时长均不设产品上限。
    pub(crate) fn record_model_request(&self) -> Result<AgentBudgetSnapshot, String> {
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
        let mut state = self
            .state
            .lock()
            .map_err(|_| "AI 预算状态已损坏".to_string())?;
        state.tool_calls = state.tool_calls.saturating_add(1);
        state.local_scan_bytes = state.local_scan_bytes.saturating_add(scan_bytes);
        state.elapsed_seconds = self.started_at.elapsed().as_secs();
        Ok(*state)
    }

    /// 为 Argus 内部强制校验预留本地扫描量，但不把它重复计为一次模型工具调用。
    ///
    /// 报告提交工具会在主分析完成后重新打开证据来源；该读取属于同一次工具调用的可信后处理，
    /// 只累计展示扫描量，不再形成会话级中断边界。
    pub(crate) fn reserve_internal_scan(
        &self,
        scan_bytes: u64,
    ) -> Result<AgentBudgetSnapshot, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "AI 预算状态已损坏".to_string())?;
        state.local_scan_bytes = state.local_scan_bytes.saturating_add(scan_bytes);
        state.elapsed_seconds = self.started_at.elapsed().as_secs();
        Ok(*state)
    }

    /// 用工具执行后获得的真实扫描量替换入口处预留量。
    ///
    /// 入口预留用于提前展示预计读取量，执行后核算用于覆盖来源大小未知、压缩后膨胀等情况。
    /// 累计扫描量仅用于状态栏审计，不再拒绝后续工具调用或终止分析。
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

    /// 记录实际暴露给模型的日志原文字节数，不再以会话累计值阻断后续取证。
    ///
    /// 单次工具仍负责按自身契约裁剪结果，避免一个响应意外挤占整个模型上下文；这里的累计值
    /// 只用于状态栏审计与报告分析成本，不代表上传额度。
    pub(crate) fn consume_raw_log_bytes(
        &self,
        bytes: usize,
    ) -> Result<AgentBudgetSnapshot, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "AI 预算状态已损坏".to_string())?;
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
    /// 固定分析阶段的结构化状态变化，只供右侧悬浮时间线展示。
    Stage(AgentAnalysisStageEvent),
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

/// 单个事件签名在一个来源中的精确重复统计。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EventOccurrenceSummary {
    /// 归一化首行的总出现次数。
    pub occurrence_count: usize,
    /// 最多二十个事件首行位置。
    pub occurrence_lines: Vec<usize>,
    /// 与 `occurrence_lines` 一一对应的原始行内容指纹；不持久化日志正文。
    pub occurrence_fingerprints: Vec<[u8; 32]>,
}

/// 会话内有界日志读取器缓存，复用已经完成的解压、编码检测和行索引。
#[derive(Debug, Default)]
pub(crate) struct AgentLogReaderCache {
    /// 按最近使用顺序保存 `(source_ref, reader)`，队首为最久未使用项。
    entries: VecDeque<(String, LogReaderHandle)>,
}

impl AgentLogReaderCache {
    /// 获取并提升一个缓存读取器的最近使用顺序。
    ///
    /// 参数说明：
    /// - `source_ref`：当前会话中的不透明来源引用。
    ///
    /// 返回值：命中时返回共享底层资源的轻量克隆，否则返回 `None`。
    pub(crate) fn get(&mut self, source_ref: &str) -> Option<LogReaderHandle> {
        let index = self
            .entries
            .iter()
            .position(|(cached_ref, _)| cached_ref == source_ref)?;
        let entry = self.entries.remove(index)?;
        let reader = entry.1.clone();
        self.entries.push_back(entry);
        Some(reader)
    }

    /// 插入或替换日志读取器，并淘汰最久未使用项。
    ///
    /// 参数说明：
    /// - `source_ref`：读取器对应的会话来源；
    /// - `reader`：已经完成打开和索引的日志句柄。
    pub(crate) fn insert(&mut self, source_ref: String, reader: LogReaderHandle) {
        if let Some(index) = self
            .entries
            .iter()
            .position(|(cached_ref, _)| cached_ref == &source_ref)
        {
            self.entries.remove(index);
        }
        self.entries.push_back((source_ref, reader));
        while self.entries.len() > MAX_AGENT_READER_CACHE_ENTRIES {
            self.entries.pop_front();
        }
    }

    /// 返回当前缓存条目数，供回归测试验证有界淘汰行为。
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

/// 会话内有界事件重复统计缓存，按最近使用顺序淘汰旧签名。
#[derive(Debug, Default)]
pub(crate) struct AgentEventOccurrenceCache {
    /// 按最近使用顺序保存 `((source_ref, signature), summary)`。
    entries: VecDeque<((String, String), EventOccurrenceSummary)>,
}

impl AgentEventOccurrenceCache {
    /// 获取并提升一个事件签名统计的最近使用顺序。
    ///
    /// 参数说明：
    /// - `key`：来源引用与归一化事件签名组成的缓存键。
    pub(crate) fn get(&mut self, key: &(String, String)) -> Option<EventOccurrenceSummary> {
        let index = self
            .entries
            .iter()
            .position(|(cached_key, _)| cached_key == key)?;
        let entry = self.entries.remove(index)?;
        let summary = entry.1.clone();
        self.entries.push_back(entry);
        Some(summary)
    }

    /// 插入或替换精确事件统计，并保持固定容量。
    ///
    /// 参数说明：
    /// - `key`：来源引用与事件签名；
    /// - `summary`：全文件扫描得到的精确统计。
    pub(crate) fn insert(&mut self, key: (String, String), summary: EventOccurrenceSummary) {
        if let Some(index) = self
            .entries
            .iter()
            .position(|(cached_key, _)| cached_key == &key)
        {
            self.entries.remove(index);
        }
        self.entries.push_back((key, summary));
        while self.entries.len() > MAX_EVENT_OCCURRENCE_CACHE_ENTRIES {
            self.entries.pop_front();
        }
    }

    /// 返回当前缓存条目数，供工具回归测试确认默认快速路径不会生成统计。
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// 返回缓存是否为空，供测试验证未请求统计时不会触发全文件扫描。
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// 工具和编排 Hook 共享的会话运行上下文。
pub(crate) struct AgentOperationContext {
    /// 不可变来源范围。
    pub scope: Arc<SourceScopeSnapshot>,
    /// 统一资源预算。
    pub budget: Arc<AgentBudget>,
    /// 固定十二阶段的单调状态跟踪器。
    pub stage_tracker: Mutex<AgentAnalysisStageTracker>,
    /// 取消令牌。
    pub cancellation: CancellationToken,
    /// 发送给 UI 的事件通道。
    pub event_sender: async_channel::Sender<AgentEvent>,
    /// 最终报告暂存槽。
    pub report: Mutex<Option<DiagnosticReport>>,
    /// 会话内大型工具结果制品；完整内容不进入轨迹，模型按 ID 分页读取。
    pub artifacts: Mutex<HashMap<String, String>>,
    /// 会话内有界日志读取器缓存；会话结束即释放，不持久化日志内容。
    pub log_reader_cache: Mutex<AgentLogReaderCache>,
    /// 已完成的同源事件签名统计，避免相同事件重复触发全文件扫描。
    pub event_occurrence_cache: Mutex<AgentEventOccurrenceCache>,
    /// 由搜索或上下文工具实际返回过的证据行及内容指纹；报告只能引用这些已观察内容。
    pub evidence_ranges: AgentEvidenceStore,
    /// 主分析已经本地复读通过的脱敏证据片段；独立复核直接继承，不重新打开日志来源。
    pub trusted_evidence_excerpts:
        Mutex<HashMap<(String, usize, usize), Option<EvidenceDisplayExcerpt>>>,
    /// 实际获取过分析说明的配置名称，最终报告会自动合并这些名称。
    pub used_log_profiles: Mutex<BTreeSet<String>>,
    /// 用户原始问题，提交报告时由可信会话层填充。
    pub question: String,
    /// 主分析阶段已经注入过的追加提示；只在同阶段模型重试时重放，永不持久化。
    pub accepted_user_messages: Mutex<Vec<AgentUserMessage>>,
    /// 当前是否处于使用全新模型上下文执行的独立复核阶段。
    pub is_independent_review: AtomicBool,
    /// 已排队但尚未注入模型上下文的用户提示数量。
    pub pending_user_messages: Arc<AtomicUsize>,
}

/// 会话内已经由确定性工具返回给模型的证据行内容指纹集合。
///
/// 内容按来源和 1 基行号保存 SHA-256，不持久化日志正文；报告提交时既要求引用行全部
/// 被工具实际观察过，也要求新建读取器复读到的当前内容与观察时一致。这样即使日志在分析中
/// 被同等行数的新内容覆盖，也不会把已变化内容错误地当作原始证据。
#[derive(Debug, Default)]
pub(crate) struct AgentEvidenceStore {
    /// `source_ref -> (1 基行号 -> 原始行 SHA-256)` 的会话内索引。
    lines: Mutex<HashMap<String, BTreeMap<usize, [u8; 32]>>>,
}

impl AgentEvidenceStore {
    /// 计算一行原始日志内容的稳定 SHA-256；脱敏和裁剪前调用才能绑定真实来源状态。
    pub(crate) fn fingerprint_text(text: &str) -> [u8; 32] {
        Sha256::digest(text.as_bytes()).into()
    }

    /// 登记工具实际返回的一条有效 1 基行号及其原始内容指纹。
    pub(crate) fn record_fingerprint(
        &self,
        source_ref: &str,
        line: usize,
        fingerprint: [u8; 32],
    ) -> Result<(), String> {
        if source_ref.is_empty() || line == 0 {
            return Err("AI 证据行无效".to_string());
        }
        self.lines
            .lock()
            .map_err(|_| "AI 证据登记状态已损坏".to_string())?
            .entry(source_ref.to_string())
            .or_default()
            .insert(line, fingerprint);
        Ok(())
    }

    /// 登记一条原始日志文本；仅供无需额外携带指纹的内部调用和回归测试使用。
    #[cfg(test)]
    pub(crate) fn record_text(
        &self,
        source_ref: &str,
        line: usize,
        text: &str,
    ) -> Result<(), String> {
        self.record_fingerprint(source_ref, line, Self::fingerprint_text(text))
    }

    /// 判断报告范围内的每一行是否都由确定性工具实际返回过。
    pub(crate) fn contains(
        &self,
        source_ref: &str,
        start_line: usize,
        end_line: usize,
    ) -> Result<bool, String> {
        if start_line == 0 || end_line < start_line {
            return Ok(false);
        }
        let lines = self
            .lines
            .lock()
            .map_err(|_| "AI 证据登记状态已损坏".to_string())?;
        let Some(recorded_lines) = lines.get(source_ref) else {
            return Ok(false);
        };
        let expected_count = end_line.saturating_sub(start_line).saturating_add(1);
        Ok(recorded_lines.range(start_line..=end_line).count() == expected_count)
    }

    /// 校验新鲜复读的行内容是否与模型观察时登记的指纹逐行一致。
    pub(crate) fn matches_lines<'a, I>(
        &self,
        source_ref: &str,
        current_lines: I,
    ) -> Result<bool, String>
    where
        I: IntoIterator<Item = (usize, &'a str)>,
    {
        let lines = self
            .lines
            .lock()
            .map_err(|_| "AI 证据登记状态已损坏".to_string())?;
        let Some(recorded_lines) = lines.get(source_ref) else {
            return Ok(false);
        };
        for (line, text) in current_lines {
            let Some(recorded_fingerprint) = recorded_lines.get(&line) else {
                return Ok(false);
            };
            if recorded_fingerprint != &Self::fingerprint_text(text) {
                return Ok(false);
            }
        }
        Ok(true)
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

    /// 向阶段时间线发布全部阶段快照，供窗口首次建立时完成初始化。
    pub(crate) fn publish_analysis_stages(&self) {
        let Ok(tracker) = self.stage_tracker.lock() else {
            return;
        };
        for event in tracker.snapshots() {
            let _ = self.event_sender.try_send(AgentEvent::Stage(event));
        }
    }

    /// 按固定顺序推进分析阶段；重复或倒序请求被安全忽略，跨阶段请求会被拒绝。
    pub(crate) fn advance_analysis_stage(&self, stage: AgentAnalysisStage) -> Result<(), String> {
        self.advance_analysis_stage_with_summary(stage, None)
    }

    /// 单调推进分析阶段，并把模型提供的摘要保存为刚完成阶段的结果。
    pub(crate) fn advance_analysis_stage_with_summary(
        &self,
        stage: AgentAnalysisStage,
        completed_summary: Option<String>,
    ) -> Result<(), String> {
        let events = self
            .stage_tracker
            .lock()
            .map_err(|_| "分析阶段状态已损坏".to_string())?
            .advance(stage, completed_summary)?;
        for event in events {
            let _ = self.event_sender.try_send(AgentEvent::Stage(event));
        }
        Ok(())
    }

    /// 完成当前阶段，通常在最终报告已经生成后关闭最后一个加载动画。
    pub(crate) fn complete_analysis_stage(
        &self,
        completed_summary: impl Into<String>,
    ) -> Result<(), String> {
        let events = self
            .stage_tracker
            .lock()
            .map_err(|_| "分析阶段状态已损坏".to_string())?
            .complete_current(Some(completed_summary.into()));
        for event in events {
            let _ = self.event_sender.try_send(AgentEvent::Stage(event));
        }
        Ok(())
    }

    /// 在每个工具入口执行取消、复核阶段扫描隔离和无上限调用计数。
    pub(crate) fn begin_tool(&self, tool_name: &str, scan_bytes: u64) -> Result<(), String> {
        if self.cancellation.is_cancelled() {
            return Err("会话已取消".to_string());
        }
        if self
            .is_independent_review
            .load(std::sync::atomic::Ordering::Acquire)
            && scan_bytes > 0
        {
            return Err(
                "独立复核沿用主分析的可信证据和日志缓存，不允许重新扫描日志来源".to_string(),
            );
        }
        let budget = self.budget.record_tool_call(scan_bytes)?;
        let inferred_stage = match tool_name {
            "search_logs" | "search_logs_batch" | "sample_log" => {
                Some(AgentAnalysisStage::SearchAnomalies)
            }
            "read_log_context" | "extract_event_blocks" => Some(AgentAnalysisStage::ExtractContext),
            "run_log_pipeline" | "aggregate_log_events" | "run_analyzer" => {
                Some(AgentAnalysisStage::BuildTimeline)
            }
            "submit_diagnostic_report"
                if self
                    .is_independent_review
                    .load(std::sync::atomic::Ordering::Acquire) =>
            {
                Some(AgentAnalysisStage::GenerateReport)
            }
            "submit_diagnostic_report" => Some(AgentAnalysisStage::ValidateEvidence),
            _ => None,
        };
        if let Some(stage) = inferred_stage {
            self.advance_analysis_stage(stage)?;
        }
        let _ = self.event_sender.try_send(AgentEvent::Budget(budget));
        self.trace(
            AgentTraceKind::Tool,
            format!("调用 {tool_name}"),
            "参数已通过来源范围与数据安全校验",
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
                priority: profile.priority,
                matchers: profile.matchers.clone(),
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

    /// 验证模型、工具、原文和本地扫描都只累计用量，不再形成会话终止上限。
    #[test]
    fn balanced_budget_tracks_unlimited_calls_and_raw_log_bytes() {
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
            .consume_raw_log_bytes(512 * 1024)
            .expect("原文达到旧累计上限时仍应成功");
        raw_budget
            .consume_raw_log_bytes(3 * 1024 * 1024)
            .expect("超过旧累计上限的后续原文不应被阻断");
        assert_eq!(raw_budget.snapshot().raw_log_bytes, 3_670_016);
        budget
            .record_tool_call(20 * 1024 * 1024 * 1024 + 1)
            .expect("超过旧扫描上限后仍应继续累计");
    }

    /// 验证工具完成后的真实扫描量会替换入口预留量，超过旧上限后仍只累计不阻断。
    #[test]
    fn tool_scan_reconciliation_uses_actual_bytes() {
        let budget = AgentBudget::balanced();
        budget.record_tool_call(64).expect("入口预留应成功");
        let snapshot = budget
            .reconcile_tool_scan(64, 128)
            .expect("实际扫描量应成功替换预留计数");
        assert_eq!(snapshot.local_scan_bytes, 128);

        let exceeded = AgentBudget::balanced();
        exceeded.record_tool_call(1).expect("入口预留应成功");
        exceeded
            .reconcile_tool_scan(1, 20 * 1024 * 1024 * 1024 + 1)
            .expect("累计扫描量不再形成会话终止上限");
        assert_eq!(
            exceeded.snapshot().local_scan_bytes,
            20 * 1024 * 1024 * 1024 + 1
        );
    }

    /// 验证固定阶段只能逐个推进，跨越阶段会被拒绝且最后阶段可显式收尾。
    #[test]
    fn analysis_stage_tracker_advances_monotonically() {
        let mut tracker = AgentAnalysisStageTracker::new(
            3,
            2,
            "已扫描 12 个日志文件".to_string(),
            "已匹配 2 种日志类型说明".to_string(),
        );
        let initial = tracker.snapshots();
        assert_eq!(initial[0].status, AgentAnalysisStageStatus::Completed);
        assert_eq!(initial[0].elapsed_seconds, 3);
        assert_eq!(
            initial[0].result_summary.as_deref(),
            Some("已扫描 12 个日志文件")
        );
        assert_eq!(initial[1].status, AgentAnalysisStageStatus::Completed);
        assert_eq!(initial[1].elapsed_seconds, 2);
        assert_eq!(initial[2].status, AgentAnalysisStageStatus::Running);

        let error = tracker
            .advance(
                AgentAnalysisStage::ExtractContext,
                Some("已拆解启动失败问题".to_string()),
            )
            .expect_err("跨越计划和检索阶段必须被拒绝");
        assert!(error.contains("下一阶段只能是“建立计划与覆盖清单”"));

        let events = tracker
            .advance(
                AgentAnalysisStage::BuildPlan,
                Some("已拆解启动失败问题".to_string()),
            )
            .expect("紧邻阶段应推进成功");
        assert_eq!(events.last().unwrap().stage, AgentAnalysisStage::BuildPlan);
        assert_eq!(
            events.last().unwrap().status,
            AgentAnalysisStageStatus::Running
        );
        assert_eq!(
            events[0].result_summary.as_deref(),
            Some("已拆解启动失败问题")
        );
        assert!(
            tracker
                .advance(AgentAnalysisStage::BreakDownQuestion, None)
                .expect("倒序阶段请求应保持幂等")
                .is_empty(),
            "倒序阶段请求不能让时间线状态回退"
        );

        let events = tracker
            .advance(AgentAnalysisStage::SearchAnomalies, None)
            .expect("后续紧邻阶段应推进成功");
        assert_eq!(events[0].stage, AgentAnalysisStage::BuildPlan);
        let events = tracker
            .advance(AgentAnalysisStage::ExtractContext, None)
            .expect("上下文阶段应按顺序推进成功");
        assert_eq!(
            events.last().map(|event| event.stage),
            Some(AgentAnalysisStage::ExtractContext)
        );

        let completed = tracker.complete_current(Some("已提取关键异常上下文".to_string()));
        assert_eq!(completed[0].stage, AgentAnalysisStage::ExtractContext);
        assert_eq!(completed[0].status, AgentAnalysisStageStatus::Completed);
        assert_eq!(
            completed[0].result_summary.as_deref(),
            Some("已提取关键异常上下文")
        );
        assert!(tracker.complete_current(None).is_empty());
    }

    /// 验证报告只能引用工具已返回且内容指纹一致的同来源连续行。
    #[test]
    fn evidence_store_rejects_unobserved_ranges() {
        let evidence = AgentEvidenceStore::default();
        for line in 10..=20 {
            evidence
                .record_text("opaque-source", line, &format!("line-{line}"))
                .expect("有效证据应登记成功");
        }
        assert!(evidence.contains("opaque-source", 12, 18).unwrap());
        assert!(!evidence.contains("opaque-source", 9, 18).unwrap());
        assert!(!evidence.contains("opaque-source", 12, 21).unwrap());
        assert!(!evidence.contains("another-source", 12, 18).unwrap());
        assert!(
            evidence
                .matches_lines(
                    "opaque-source",
                    (12..=18)
                        .map(|line| (line, format!("line-{line}")))
                        .collect::<Vec<_>>()
                        .iter()
                        .map(|(line, text)| (*line, text.as_str())),
                )
                .unwrap()
        );
        assert!(
            !evidence
                .matches_lines("opaque-source", [(12, "changed")])
                .unwrap(),
            "相同行号被新内容覆盖后必须拒绝作为原证据"
        );
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
