//! 文件职责：定义 AI 分析会话状态、范围快照、资源预算、轨迹事件和追加消息。
//! 创建日期：2026-07-15
//! 修改日期：2026-07-17
//! 作者：Argus 开发团队
//! 主要功能：把来源树固化为不可变授权范围，并统一记录 Agent 日志访问、调用、Token、扫描量、独立复核和取消边界。

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize},
};
use std::time::Instant;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agent::log_access::AgentLogAccess;

use crate::agent::report::{DiagnosticReport, EvidenceDisplayExcerpt};
use crate::config::{AiConfig, LogNameMatcher, LogTypeProfile};
use crate::loader::archive::ArchivePasswordStore;
use crate::loader::{SourceId, SourceLocation, SourceRegistry};

/// Agent 会话的产品运行模式；工具层据此决定是否开放报告与阶段展示能力。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AgentSessionMode {
    /// 独立智能分析窗口使用的动态阶段和最终报告模式。
    StructuredAnalysis,
    /// 主窗口右侧助手使用的自由多轮交互模式。
    InteractiveAssistant,
}

/// 来源快照的选择范围，避免交互助手复用单根入口时意外缩小授权范围。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AgentScopeSelection {
    /// 根据当前选中节点解析一个根；保留现有智能分析行为。
    SelectedRoot(Option<SourceId>),
    /// 固化来源注册表中的全部已加载根。
    AllLoadedRoots,
}

/// 单个工具 JSON 结果上限。
pub(crate) const MAX_TOOL_RESULT_BYTES: usize = 128 * 1024;
/// 单个工具结果中的日志原文上限。
pub(crate) const MAX_TOOL_RAW_BYTES: usize = 64 * 1024;
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

/// 来源扫描是模型运行前由 Argus 确定性执行的系统阶段。
const SOURCE_SCAN_STAGE_ID: &str = "scan_sources";
/// 来源扫描阶段的用户可见标题。
const SOURCE_SCAN_STAGE_TITLE: &str = "完整扫描来源树";
/// 来源扫描摘要缺失时的保守结果。
const SOURCE_SCAN_DEFAULT_SUMMARY: &str = "来源树扫描已完成";
/// 日志类型匹配是模型运行前由 Argus 确定性执行的系统阶段。
const LOG_PROFILE_STAGE_ID: &str = "match_log_types";
/// 日志类型匹配阶段的用户可见标题。
const LOG_PROFILE_STAGE_TITLE: &str = "匹配日志类型与说明";
/// 日志类型匹配摘要缺失时的保守结果。
const LOG_PROFILE_DEFAULT_SUMMARY: &str = "日志类型与说明匹配已完成";

/// 单个分析阶段的最终结果或当前运行状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AgentAnalysisStageStatus {
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
    /// 会话内稳定的阶段标识；模型可以根据问题动态定义。
    pub stage_id: String,
    /// 用户可见阶段标题。
    pub title: String,
    /// 当前结果状态。
    pub status: AgentAnalysisStageStatus,
    /// 阶段已消耗秒数；运行态由界面在此基础上继续计时。
    pub elapsed_seconds: u64,
    /// 完成、失败或取消阶段的简短结果摘要；不包含具体思考、工具参数或日志原文。
    pub result_summary: Option<String>,
}

/// 动态阶段的内部记录。
struct TrackedAnalysisStage {
    /// 会话内稳定标识。
    stage_id: String,
    /// 用户可见标题。
    title: String,
    /// 当前状态。
    status: AgentAnalysisStageStatus,
    /// 已确认耗时。
    elapsed_seconds: u64,
    /// 完成结果摘要。
    result_summary: Option<String>,
    /// 当前运行阶段的开始时刻。
    started_at: Option<Instant>,
    /// 摘要缺失时使用的保守说明。
    default_result_summary: String,
}

/// 动态分析阶段的单会话跟踪器。
///
/// 跟踪器只保证同一时间最多一个运行阶段，并保持已经出现的阶段顺序；阶段数量、标题和
/// 推进顺序完全由模型根据当前问题决定，不再隐式补齐任何固定流程。
pub(crate) struct AgentAnalysisStageTracker {
    /// 已经实际出现的阶段，保持展示顺序。
    stages: Vec<TrackedAnalysisStage>,
}

impl AgentAnalysisStageTracker {
    /// 使用启动前已经测得的来源扫描和类型匹配结果创建跟踪器。
    ///
    /// 后续不预建任何待执行阶段，首个模型阶段由模型根据问题自行声明。
    pub(crate) fn new(
        source_scan_seconds: u64,
        profile_seconds: u64,
        source_scan_summary: String,
        profile_summary: String,
    ) -> Self {
        Self {
            stages: vec![
                TrackedAnalysisStage {
                    stage_id: SOURCE_SCAN_STAGE_ID.to_string(),
                    title: SOURCE_SCAN_STAGE_TITLE.to_string(),
                    status: AgentAnalysisStageStatus::Completed,
                    elapsed_seconds: source_scan_seconds,
                    result_summary: Some(normalize_stage_result_summary(
                        source_scan_summary,
                        SOURCE_SCAN_DEFAULT_SUMMARY,
                    )),
                    started_at: None,
                    default_result_summary: SOURCE_SCAN_DEFAULT_SUMMARY.to_string(),
                },
                TrackedAnalysisStage {
                    stage_id: LOG_PROFILE_STAGE_ID.to_string(),
                    title: LOG_PROFILE_STAGE_TITLE.to_string(),
                    status: AgentAnalysisStageStatus::Completed,
                    elapsed_seconds: profile_seconds,
                    result_summary: Some(normalize_stage_result_summary(
                        profile_summary,
                        LOG_PROFILE_DEFAULT_SUMMARY,
                    )),
                    started_at: None,
                    default_result_summary: LOG_PROFILE_DEFAULT_SUMMARY.to_string(),
                },
            ],
        }
    }

    /// 返回已经实际出现的全部阶段快照。
    pub(crate) fn snapshots(&self) -> Vec<AgentAnalysisStageEvent> {
        self.stages.iter().map(Self::event).collect()
    }

    /// 完成当前阶段并启动模型声明的任意新阶段。
    ///
    /// 已出现的阶段标识按幂等请求处理，防止网络重试重复增加时间线节点；新阶段不与任何
    /// 固定清单比较，因此模型可以自由选择阶段数量、名称和顺序。
    pub(crate) fn advance_dynamic(
        &mut self,
        stage_id: String,
        title: String,
        completed_summary: Option<String>,
        default_result_summary: String,
    ) -> Result<Vec<AgentAnalysisStageEvent>, String> {
        validate_dynamic_stage(&stage_id, &title)?;
        if self.stages.iter().any(|stage| stage.stage_id == stage_id) {
            return Ok(Vec::new());
        }

        let mut events = Vec::new();
        if let Some(current) = self
            .stages
            .iter_mut()
            .find(|stage| stage.status == AgentAnalysisStageStatus::Running)
        {
            current.status = AgentAnalysisStageStatus::Completed;
            current.elapsed_seconds = current
                .started_at
                .take()
                .map(|started_at| started_at.elapsed().as_secs())
                .unwrap_or(current.elapsed_seconds);
            current.result_summary = Some(normalize_stage_result_summary(
                completed_summary.unwrap_or_default(),
                &current.default_result_summary,
            ));
            events.push(Self::event(current));
        }

        self.stages.push(TrackedAnalysisStage {
            stage_id,
            title,
            status: AgentAnalysisStageStatus::Running,
            elapsed_seconds: 0,
            result_summary: None,
            started_at: Some(Instant::now()),
            default_result_summary,
        });
        if let Some(current) = self.stages.last() {
            events.push(Self::event(current));
        }
        Ok(events)
    }

    /// 完成当前阶段；最终报告成功后用于关闭最后一个加载动画。
    pub(crate) fn complete_current(
        &mut self,
        completed_summary: Option<String>,
    ) -> Vec<AgentAnalysisStageEvent> {
        let Some(current) = self
            .stages
            .iter_mut()
            .find(|stage| stage.status == AgentAnalysisStageStatus::Running)
        else {
            return Vec::new();
        };
        current.status = AgentAnalysisStageStatus::Completed;
        current.elapsed_seconds = current
            .started_at
            .take()
            .map(|started_at| started_at.elapsed().as_secs())
            .unwrap_or(current.elapsed_seconds);
        current.result_summary = Some(normalize_stage_result_summary(
            completed_summary.unwrap_or_default(),
            &current.default_result_summary,
        ));
        vec![Self::event(current)]
    }

    /// 构造单个阶段事件；运行态耗时包含当前已经经过的时间。
    fn event(stage: &TrackedAnalysisStage) -> AgentAnalysisStageEvent {
        let elapsed_seconds = if stage.status == AgentAnalysisStageStatus::Running {
            stage
                .started_at
                .map(|started_at| started_at.elapsed().as_secs())
                .unwrap_or(stage.elapsed_seconds)
        } else {
            stage.elapsed_seconds
        };
        AgentAnalysisStageEvent {
            stage_id: stage.stage_id.clone(),
            title: stage.title.clone(),
            status: stage.status,
            elapsed_seconds,
            result_summary: stage.result_summary.clone(),
        }
    }
}

/// 校验模型提供的动态阶段标识和标题，防止不可见字符破坏界面稳定键与布局。
fn validate_dynamic_stage(stage_id: &str, title: &str) -> Result<(), String> {
    let valid_id = !stage_id.is_empty()
        && stage_id.len() <= 96
        && stage_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'/'));
    if !valid_id {
        return Err("阶段标识只能包含字母、数字、下划线、短横线或斜杠，且最长 96 字节".to_string());
    }
    let compact_title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact_title.is_empty() || compact_title.len() > 120 {
        return Err("阶段标题不能为空且最长 120 字节".to_string());
    }
    Ok(())
}

/// 规范阶段结果摘要并保留显式换行，避免模型内容破坏时间线布局或复制大段日志。
fn normalize_stage_result_summary(summary: String, default_summary: &str) -> String {
    let multiline = summary
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let value = if multiline.is_empty() {
        default_summary.to_string()
    } else {
        multiline
    };
    truncate_utf8_with_ellipsis(value, 480)
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
    /// 日志类型规则使用的原始根内相对路径；多根展示前缀不得改变用户既有匹配语义。
    pub profile_match_path: String,
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
    /// 当前进程内压缩包密码快照，只供底层读取器使用。
    pub archive_passwords: ArchivePasswordStore,
    /// 是否允许把工具返回的必要日志原文发送给模型。
    pub allow_raw_log_content: bool,
}

impl SourceScopeSnapshot {
    /// 按明确范围选择固化来源树；交互助手使用全部根，固定分析继续使用单根。
    pub(crate) fn from_registry_selection(
        registry: &SourceRegistry,
        selection: AgentScopeSelection,
        config: &AiConfig,
        default_encoding: String,
        archive_passwords: ArchivePasswordStore,
    ) -> Result<Self, String> {
        let root_ids = resolve_scope_root_ids(registry, selection)?;
        let root_labels = root_ids
            .iter()
            .filter_map(|root_id| registry.node(*root_id).map(|root| root.label.clone()))
            .collect::<Vec<_>>();
        if root_labels.len() != root_ids.len() {
            return Err("来源根已经失效".to_string());
        }
        let include_root_prefix = matches!(selection, AgentScopeSelection::AllLoadedRoots);
        let root_display_labels = unique_root_display_labels(&root_ids, &root_labels);
        let mut profile_snapshots = build_profile_snapshots(&config.log_profiles);
        let mut sources = Vec::new();
        for source_id in registry.tree_order_source_ids() {
            let Some(node) = registry.node(*source_id) else {
                continue;
            };
            let Some(root_id) = registry.root_id_for(*source_id) else {
                continue;
            };
            if !node.kind.is_log_candidate() || !root_ids.contains(&root_id) {
                continue;
            }
            let inner_path = relative_path_from_root(registry, root_id, *source_id);
            let relative_path = if include_root_prefix {
                let root_label = root_display_labels
                    .get(&root_id)
                    .map(String::as_str)
                    .unwrap_or("来源");
                if inner_path.is_empty() {
                    root_label.to_string()
                } else {
                    format!("{root_label}/{inner_path}")
                }
            } else {
                inner_path.clone()
            };
            let profile_id = select_profile(&config.log_profiles, &node.label, &inner_path)
                .map(|profile| profile.profile_id.clone());
            sources.push(SnapshotSource {
                source_ref: Uuid::new_v4().to_string(),
                source_id: *source_id,
                file_name: node.label.clone(),
                relative_path,
                profile_match_path: inner_path,
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
            root_label: if root_labels.len() == 1 {
                root_labels[0].clone()
            } else {
                format!("全部已加载来源（{} 个根）", root_labels.len())
            },
            sources: Arc::new(sources),
            profiles: Arc::new(profile_snapshots),
            default_encoding,
            archive_passwords,
            allow_raw_log_content: config.allow_raw_log_content,
        })
    }

    /// 按不透明引用解析当前不可变来源快照；该方法只读取 Agent 会话数据，不访问主窗口状态。
    pub(crate) fn source(&self, source_ref: &str) -> Option<&SnapshotSource> {
        self.sources
            .iter()
            .find(|source| source.source_ref == source_ref)
    }
}

/// 为多根来源生成稳定且互不重复的会话展示名；同名根按来源树顺序追加序号。
fn unique_root_display_labels(
    root_ids: &[SourceId],
    root_labels: &[String],
) -> HashMap<SourceId, String> {
    let mut totals = HashMap::<&str, usize>::new();
    for label in root_labels {
        *totals.entry(label.as_str()).or_default() += 1;
    }
    let mut seen = HashMap::<&str, usize>::new();
    root_ids
        .iter()
        .copied()
        .zip(root_labels)
        .map(|(root_id, label)| {
            let sequence = seen.entry(label.as_str()).or_default();
            *sequence += 1;
            let display = if totals.get(label.as_str()).copied().unwrap_or(0) > 1 {
                format!("{label} ({sequence})")
            } else {
                label.clone()
            };
            (root_id, display)
        })
        .collect()
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
    /// 动态分析阶段的结构化状态变化，只供右侧悬浮时间线展示。
    Stage(AgentAnalysisStageEvent),
    /// 模型思考或可见正文的流式增量；同类相邻事件由 UI 合并显示。
    StreamDelta(AgentStreamKind, String),
    /// 用户提示已被下一模型请求消费。
    UserMessageConsumed(String),
    /// 用户提示在最终报告或取消边界到达过晚，未进入模型上下文。
    UserMessageRejected(String, String),
    /// 最终结构化报告和可选持久化路径。
    Report(DiagnosticReport, Option<String>),
    /// 交互助手一轮回答完成，并携带本轮本地验证引用和实际进入模型的用户消息。
    AssistantCompleted {
        /// 模型最终可见正文。
        output: String,
        /// 当前回答已经本地复读通过的日志引用。
        citations: Vec<crate::agent::report::AssistantCitation>,
        /// 初始问题和在模型边界成功消费的补充消息。
        accepted_user_messages: Vec<String>,
        /// 构造本轮模型历史时是否因上下文容量移除了较早轮次。
        history_was_trimmed: bool,
    },
    /// 交互助手的可恢复模型故障即将重试；界面应丢弃本次尝试尚未完成的流式正文。
    AssistantAttemptReset,
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
    /// 当前运行模式；交互助手不注册阶段与报告工具，但不绕过任何数据安全边界。
    pub session_mode: AgentSessionMode,
    /// 不可变来源范围。
    pub scope: Arc<SourceScopeSnapshot>,
    /// 统一资源预算。
    pub budget: Arc<AgentBudget>,
    /// 模型动态规划阶段的顺序状态跟踪器。
    pub stage_tracker: Mutex<AgentAnalysisStageTracker>,
    /// 取消令牌。
    pub cancellation: CancellationToken,
    /// 发送给 UI 的事件通道。
    pub event_sender: async_channel::Sender<AgentEvent>,
    /// 最终报告暂存槽。
    pub report: Mutex<Option<DiagnosticReport>>,
    /// 会话内大型工具结果制品；完整内容不进入轨迹，模型按 ID 分页读取。
    pub artifacts: Mutex<HashMap<String, String>>,
    /// Agent 专用日志目录和只读访问服务；统一复用目录索引、解压结果与行读取器。
    pub log_access: AgentLogAccess,
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
    /// 交互助手当前回答已经通过本地验证的引用；固定分析模式保持为空。
    pub assistant_citations: Mutex<Vec<crate::agent::report::AssistantCitation>>,
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

    /// 启动模型自由定义的分析阶段，并完成此前运行阶段。
    ///
    /// 参数说明：
    /// - `stage_id`：模型在当前上下文内生成的稳定短标识；调用方负责增加主分析或复核命名空间。
    /// - `title`：右侧进度卡片展示的阶段标题。
    /// - `completed_summary`：刚完成阶段的多行客观摘要。
    pub(crate) fn advance_dynamic_analysis_stage(
        &self,
        stage_id: String,
        title: String,
        completed_summary: Option<String>,
    ) -> Result<(), String> {
        let events = self
            .stage_tracker
            .lock()
            .map_err(|_| "分析阶段状态已损坏".to_string())?
            .advance_dynamic(stage_id, title, completed_summary, "阶段已完成".to_string())?;
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
        // 工具类型不再隐式决定分析阶段。模型可以根据问题复杂度自由组织搜索、读取、
        // 聚合与验证步骤，右侧时间线只响应显式 `set_analysis_stage` 调用。
        let _ = self.event_sender.try_send(AgentEvent::Budget(budget));
        self.trace(
            AgentTraceKind::Tool,
            format!("调用 {tool_name}"),
            "参数已通过来源范围与数据安全校验",
        );
        Ok(())
    }
}

/// 把来源选择解析为稳定根 ID 列表；全部来源模式保持注册表原始根顺序。
fn resolve_scope_root_ids(
    registry: &SourceRegistry,
    selection: AgentScopeSelection,
) -> Result<Vec<SourceId>, String> {
    match selection {
        AgentScopeSelection::SelectedRoot(Some(source_id)) => registry
            .root_id_for(source_id)
            .map(|root_id| vec![root_id])
            .ok_or_else(|| "当前选中来源不存在，无法确定 AI 分析范围".to_string()),
        AgentScopeSelection::SelectedRoot(None) if registry.root_ids().len() == 1 => {
            Ok(vec![registry.root_ids()[0]])
        }
        AgentScopeSelection::SelectedRoot(None) if registry.root_ids().is_empty() => {
            Err("请先加载日志来源".to_string())
        }
        AgentScopeSelection::SelectedRoot(None) => {
            Err("存在多个来源根，请先在来源树中选择要分析的范围".to_string())
        }
        AgentScopeSelection::AllLoadedRoots if registry.root_ids().is_empty() => {
            Err("请先加载日志来源".to_string())
        }
        AgentScopeSelection::AllLoadedRoots => Ok(registry.root_ids().to_vec()),
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

    /// 验证阶段由模型动态创建、重复标识幂等且多行结果摘要得到保留。
    #[test]
    fn analysis_stage_tracker_accepts_dynamic_model_plan() {
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
        assert_eq!(initial.len(), 2);

        let events = tracker
            .advance_dynamic(
                "extract_context".to_string(),
                "提取事件上下文".to_string(),
                None,
                "候选事件上下文已提取".to_string(),
            )
            .expect("模型应能直接选择当前问题需要的阶段");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].stage_id, "extract_context");
        assert_eq!(events[0].status, AgentAnalysisStageStatus::Running);

        let events = tracker
            .advance_dynamic(
                "primary/compare_baseline".to_string(),
                "比较正常基线".to_string(),
                Some("已定位启动失败上下文\n已确定异常时间窗".to_string()),
                "阶段已完成".to_string(),
            )
            .expect("自由命名的新阶段应推进成功");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].stage_id, "extract_context");
        assert_eq!(events.last().unwrap().stage_id, "primary/compare_baseline");
        assert_eq!(
            events.last().unwrap().status,
            AgentAnalysisStageStatus::Running
        );
        assert_eq!(
            events[0].result_summary.as_deref(),
            Some("已定位启动失败上下文\n已确定异常时间窗")
        );
        assert!(
            tracker
                .advance_dynamic(
                    "primary/compare_baseline".to_string(),
                    "比较正常基线".to_string(),
                    None,
                    "阶段已完成".to_string(),
                )
                .expect("重复阶段请求应保持幂等")
                .is_empty(),
            "网络重试不能重复增加时间线节点"
        );

        let completed = tracker.complete_current(Some("已完成正常与异常样本对比".to_string()));
        assert_eq!(completed[0].stage_id, "primary/compare_baseline");
        assert_eq!(completed[0].status, AgentAnalysisStageStatus::Completed);
        assert_eq!(
            completed[0].result_summary.as_deref(),
            Some("已完成正常与异常样本对比")
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
        let snapshot = SourceScopeSnapshot::from_registry_selection(
            &registry,
            AgentScopeSelection::SelectedRoot(None),
            &config,
            "UTF-8".to_string(),
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
