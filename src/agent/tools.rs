//! 文件职责：实现通用智能体限定在工作目录内的三个领域无关工具。
//! 创建日期：2026-09-14
//! 作者：Argus 开发团队
//! 主要功能：工作区来源清单、越界拒绝的文件读取和白名单加确认闸门的 bash 执行。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use uuid::Uuid;

use rig_core::tool::Tool;

use crate::agent::session::{
    AgentEvent, AgentOperationContext, SnapshotSource, truncate_utf8_with_ellipsis,
};
use crate::log_io::encoding_detector::decode_log_bytes;

/// read_file 单次返回内容上限；超出部分截断并提示续读偏移。
const MAX_READ_FILE_BYTES: usize = 64 * 1024;
/// read_file 默认与最大返回行数。
const DEFAULT_READ_FILE_LINES: usize = 200;
const MAX_READ_FILE_LINES: usize = 2000;
/// list_loaded_sources 默认与最大返回条目数。
const DEFAULT_LIST_LIMIT: usize = 200;
const MAX_LIST_LIMIT: usize = 1000;
/// bash 默认超时与最大超时。
const DEFAULT_BASH_TIMEOUT_MS: u64 = 60_000;
const MAX_BASH_TIMEOUT_MS: u64 = 300_000;
/// bash stdout 与 stderr 各自的返回上限。
const MAX_BASH_STDOUT_BYTES: usize = 96 * 1024;
const MAX_BASH_STDERR_BYTES: usize = 32 * 1024;
/// bash 审批卡片等待用户答复的时长；超时默认拒绝。
pub(crate) const BASH_APPROVAL_TIMEOUT: Duration = Duration::from_secs(60);
/// 只读命令白名单；按 argv[0] 判定，白名单外命令一律转用户确认。
const READONLY_COMMAND_WHITELIST: &[&str] = &[
    "ls", "cat", "grep", "head", "tail", "wc", "find", "sort", "uniq", "awk", "sed", "cut", "tr",
    "echo", "pwd", "stat", "file", "du", "df",
];

/// 工具统一错误；错误文本面向模型，使用英文且不包含凭据或大段日志原文。
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(crate) struct AgentToolError(String);

impl AgentToolError {
    /// 创建经过长度裁剪的工具错误。
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(truncate_utf8_with_ellipsis(message.into(), 1024))
    }
}

/// 把 JsonSchema 类型转换为 OpenAI 兼容的参数 Schema。
fn schema_value<T: JsonSchema>() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(T))
        .unwrap_or_else(|_| serde_json::json!({ "type": "object" }))
}

/// 解析模型输入为工作目录内的规范路径；拒绝 `..` 越界、符号链接逃逸和不存在目标。
///
/// 返回值：`(绝对路径, 工作目录内正斜杠相对路径)`。
fn resolve_workspace_path(
    context: &AgentOperationContext,
    input: &str,
) -> Result<(PathBuf, String), AgentToolError> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err(AgentToolError::new("path must not be empty"));
    }
    if raw.contains('\0') {
        return Err(AgentToolError::new("path contains a NUL character"));
    }
    let root = &context.scope.workspace_root;
    let candidate = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        root.join(raw)
    };
    // 词法规范化先行：先剥掉工作目录前缀，再拒绝任何向上越出根的相对输入，全程不触碰文件系统。
    let relative = candidate
        .strip_prefix(root)
        .map_err(|_| AgentToolError::new(format!("path escapes the workspace directory: {raw}")))?;
    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    for component in relative.components() {
        match component {
            std::path::Component::ParentDir => {
                if parts.pop().is_none() {
                    return Err(AgentToolError::new(format!(
                        "path escapes the workspace directory: {raw}"
                    )));
                }
            }
            std::path::Component::CurDir => {}
            other => parts.push(other.as_os_str().to_os_string()),
        }
    }
    let lexical = root.join(parts.iter().collect::<PathBuf>());
    // canonicalize 解析符号链接与大小写别名；逃逸链接在这里被真实路径前缀校验拦截。
    let canonical_root = std::fs::canonicalize(&context.scope.workspace_root).map_err(|error| {
        AgentToolError::new(format!("workspace directory unavailable: {error}"))
    })?;
    let canonical = std::fs::canonicalize(&lexical)
        .map_err(|_| AgentToolError::new(format!("path does not exist in the workspace: {raw}")))?;
    if !canonical.starts_with(&canonical_root) {
        return Err(AgentToolError::new(format!(
            "path escapes the workspace directory: {raw}"
        )));
    }
    let relative = canonical
        .strip_prefix(&canonical_root)
        .map_err(|_| AgentToolError::new("path escapes the workspace directory"))?;
    let workspace_path = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    Ok((canonical, workspace_path))
}

/// 供工具输出使用的敏感信息遮蔽；模型上下文不得携带明文凭据。
pub(crate) fn redact_sensitive_text(text: &str) -> String {
    // 常见 Bearer Token 与 key=value 形态的凭据；正则按需惰性初始化。
    static PATTERNS: std::sync::OnceLock<Vec<regex::Regex>> = std::sync::OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        [
            r"(?i)bearer\s+[A-Za-z0-9._~+/=-]+",
            r#"(?i)(password|passwd|pwd|token|api[_-]?key|secret)\s*[:=]\s*[^\s,;]+"#,
        ]
        .into_iter()
        .filter_map(|pattern| regex::Regex::new(pattern).ok())
        .collect()
    });
    patterns.iter().fold(text.to_string(), |value, regex| {
        regex.replace_all(&value, "[REDACTED]").into_owned()
    })
}

// ---------------------------------------------------------------------------
// list_loaded_sources
// ---------------------------------------------------------------------------

/// 来源清单查询参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct ListLoadedSourcesArgs {
    /// 可选的工作目录相对路径前缀；只返回该目录后代中的日志文件。
    #[serde(default)]
    pub path_prefix: Option<String>,
    /// 返回条目上限，范围 1～1000。
    #[serde(default = "default_list_limit")]
    pub limit: usize,
}

/// 返回 list_loaded_sources 的默认分页大小。
fn default_list_limit() -> usize {
    DEFAULT_LIST_LIMIT
}

/// 单个来源的清单条目。
#[derive(Debug, Serialize)]
struct ListedSource {
    /// 工作目录内相对路径（正斜杠分隔）。
    path: String,
    /// 工作目录内真实绝对路径。
    absolute_path: String,
    /// 已知文件大小（字节）。
    size_bytes: Option<u64>,
    /// 命中的日志类型说明；未命中为 null。
    log_profile: Option<ListedProfile>,
}

/// 日志类型说明摘要。
#[derive(Debug, Serialize)]
struct ListedProfile {
    /// 用户配置的类型名称。
    name: String,
    /// 用户配置的日志结构化说明全文。
    description: String,
}

/// 来源清单查询结果。
#[derive(Debug, Serialize)]
pub(crate) struct ListLoadedSourcesOutput {
    /// 当前工作目录根。
    workspace_root: String,
    /// 满足前缀条件的来源总数。
    total_sources: usize,
    /// 本次实际返回条数。
    returned: usize,
    /// 是否因条目上限截断；截断时用更具体的 path_prefix 细查。
    truncated: bool,
    /// 来源清单。
    sources: Vec<ListedSource>,
}

/// 列出当前会话授权的工作区日志清单。
#[derive(Clone)]
pub(crate) struct ListLoadedSourcesTool(pub Arc<AgentOperationContext>);

impl Tool for ListLoadedSourcesTool {
    const NAME: &'static str = "list_loaded_sources";
    type Error = AgentToolError;
    type Args = ListLoadedSourcesArgs;
    type Output = ListLoadedSourcesOutput;

    fn description(&self) -> String {
        "List the log files authorized for this session. Each entry carries a workspace-relative \
        path (forward slashes), the real absolute path, size and the matched log-type guidance. \
        Use path_prefix (a workspace-relative directory prefix) to narrow large workspaces; use \
        the returned paths with read_file and bash."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        schema_value::<Self::Args>()
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, AgentToolError> {
        let limit = args.limit.clamp(1, MAX_LIST_LIMIT);
        let prefix = args
            .path_prefix
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.trim_matches('/').to_string());
        let prefix_boundary = prefix.as_deref().map(|prefix| format!("{prefix}/"));
        let mut selected: Vec<&SnapshotSource> = self
            .0
            .scope
            .sources
            .iter()
            .filter(|source| match (&prefix, &prefix_boundary) {
                (Some(prefix), Some(boundary)) => {
                    source.workspace_path == *prefix
                        || source.workspace_path.starts_with(boundary.as_str())
                }
                _ => true,
            })
            .collect();
        selected.sort_by(|left, right| left.workspace_path.cmp(&right.workspace_path));
        let total_sources = selected.len();
        let truncated = selected.len() > limit;
        selected.truncate(limit);
        let sources: Vec<ListedSource> = selected
            .into_iter()
            .map(|source| {
                let log_profile = source
                    .profile_id
                    .as_deref()
                    .and_then(|profile_id| self.0.scope.profiles.get(profile_id))
                    .map(|profile| ListedProfile {
                        name: profile.name.clone(),
                        description: profile.description.clone(),
                    });
                ListedSource {
                    path: source.workspace_path.clone(),
                    absolute_path: source.absolute_path.display().to_string(),
                    size_bytes: source.size,
                    log_profile,
                }
            })
            .collect();
        Ok(ListLoadedSourcesOutput {
            workspace_root: self.0.scope.workspace_root.display().to_string(),
            total_sources,
            returned: sources.len(),
            truncated,
            sources,
        })
    }
}

// ---------------------------------------------------------------------------
// read_file
// ---------------------------------------------------------------------------

/// 文件读取参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct ReadFileArgs {
    /// 工作目录内相对路径或工作目录内绝对路径。
    pub path: String,
    /// 起始行号（1 基）；默认 1。
    #[serde(default)]
    pub offset_line: Option<usize>,
    /// 返回行数上限，范围 1～2000；默认 200。
    #[serde(default)]
    pub max_lines: Option<usize>,
}

/// 文件读取结果。
#[derive(Debug, Serialize)]
pub(crate) struct ReadFileOutput {
    /// 工作目录内相对路径（正斜杠分隔）。
    path: String,
    /// 工作目录内真实绝对路径。
    absolute_path: String,
    /// 实际采用的解码编码。
    encoding: String,
    /// 起始行号（1 基）。
    offset_line: usize,
    /// 本次返回的行数。
    line_count: usize,
    /// 是否因字节或行数上限截断。
    truncated: bool,
    /// 截断时建议的续读起始行号。
    next_offset_line: Option<usize>,
    /// 文件正文；未授权发送原文时为空。
    content: String,
    /// 未授权发送原文时的说明。
    content_withheld: Option<String>,
}

/// 在授权与路径边界内读取文件内容。
#[derive(Clone)]
pub(crate) struct ReadFileTool(pub Arc<AgentOperationContext>);

impl Tool for ReadFileTool {
    const NAME: &'static str = "read_file";
    type Error = AgentToolError;
    type Args = ReadFileArgs;
    type Output = ReadFileOutput;

    fn description(&self) -> String {
        "Read a file inside the current workspace directory. The path must stay inside the \
        workspace; traversal, symlink escapes and absolute paths outside it are rejected. \
        Returns up to max_lines lines starting at offset_line (1-based), decoded with the \
        detected or configured encoding. When raw log content is not authorized the tool \
        returns metadata only."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        schema_value::<Self::Args>()
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, AgentToolError> {
        let (absolute_path, workspace_path) = resolve_workspace_path(&self.0, &args.path)?;
        let metadata = tokio::fs::metadata(&absolute_path)
            .await
            .map_err(|error| AgentToolError::new(format!("cannot stat file: {error}")))?;
        if !metadata.is_file() {
            return Err(AgentToolError::new(
                "path points to a directory; pass a file path",
            ));
        }
        let offset_line = args.offset_line.unwrap_or(1).max(1);
        let max_lines = args
            .max_lines
            .unwrap_or(DEFAULT_READ_FILE_LINES)
            .clamp(1, MAX_READ_FILE_LINES);
        let empty_output = |encoding: String| ReadFileOutput {
            path: workspace_path.clone(),
            absolute_path: absolute_path.display().to_string(),
            encoding,
            offset_line,
            line_count: 0,
            truncated: false,
            next_offset_line: None,
            content: String::new(),
            content_withheld: None,
        };
        if !self.0.scope.allow_raw_log_content {
            let mut output = empty_output(String::new());
            output.content_withheld = Some(
                "Raw log content is not authorized in this session; only metadata is returned. \
                Ask the user to enable raw log content in settings."
                    .to_string(),
            );
            return Ok(output);
        }
        let file = tokio::fs::File::open(&absolute_path)
            .await
            .map_err(|error| AgentToolError::new(format!("cannot open file: {error}")))?;
        let mut reader = BufReader::new(file);
        let mut skipped: usize = 0;
        // 逐行读取并跳到目标行；偏移之前的内容不进入内存。
        while skipped + 1 < offset_line {
            let mut discard = Vec::new();
            let read = reader
                .read_until(b'\n', &mut discard)
                .await
                .map_err(|error| AgentToolError::new(format!("cannot read file: {error}")))?;
            if read == 0 {
                return Ok(empty_output(self.0.scope.preferred_encoding.clone()));
            }
            skipped += 1;
        }
        let mut buffer = Vec::new();
        let mut collected_lines: usize = 0;
        let mut truncated = false;
        let mut next_line = offset_line;
        while collected_lines < max_lines {
            let mut line = Vec::new();
            let read = reader
                .read_until(b'\n', &mut line)
                .await
                .map_err(|error| AgentToolError::new(format!("cannot read file: {error}")))?;
            if read == 0 {
                break;
            }
            if buffer.len() + line.len() > MAX_READ_FILE_BYTES {
                truncated = true;
                break;
            }
            next_line += 1;
            collected_lines += 1;
            buffer.extend_from_slice(&line);
        }
        let decoded = decode_log_bytes(&buffer, &self.0.scope.preferred_encoding);
        Ok(ReadFileOutput {
            content: truncate_utf8_with_ellipsis(
                redact_sensitive_text(&decoded.text),
                MAX_READ_FILE_BYTES,
            ),
            encoding: decoded.encoding_label.clone(),
            truncated,
            next_offset_line: truncated.then_some(next_line),
            line_count: collected_lines,
            ..empty_output(String::new())
        })
    }
}

// ---------------------------------------------------------------------------
// bash
// ---------------------------------------------------------------------------

/// bash 执行参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct BashArgs {
    /// 要在工作目录内执行的命令行。
    pub command: String,
    /// 可选超时（毫秒），范围 1000～300000；默认 60000。
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// bash 执行结果。
#[derive(Debug, Serialize)]
pub(crate) struct BashOutput {
    /// 命令是否实际执行；被用户拒绝或超时未确认时为 false。
    executed: bool,
    /// 未执行时的原因说明。
    status: String,
    /// 进程退出码；未执行或被超时杀死时为 null。
    exit_code: Option<i32>,
    /// 截断后的标准输出。
    stdout: String,
    /// 截断后的标准错误。
    stderr: String,
    /// stdout 是否被截断。
    stdout_truncated: bool,
    /// stderr 是否被截断。
    stderr_truncated: bool,
}

/// bash 命令的安全分类结果。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum BashClassification {
    /// 白名单只读命令且全部路径参数都在工作目录内，直接执行。
    AutoRun,
    /// 需要用户在确认卡片中批准后才能执行。
    NeedsApproval(String),
}

/// 在受限边界内执行 shell 命令。
#[derive(Clone)]
pub(crate) struct BashTool(pub Arc<AgentOperationContext>);

impl Tool for BashTool {
    const NAME: &'static str = "bash";
    type Error = AgentToolError;
    type Args = BashArgs;
    type Output = BashOutput;

    fn description(&self) -> String {
        "Run a shell command with the workspace directory as the working directory. Read-only \
        whitelisted commands (ls, cat, grep, head, tail, wc, find, sort, uniq, awk, sed, cut, \
        tr, echo, pwd, stat, file, du, df) run automatically when every path argument stays \
        inside the workspace; anything else (other binaries, redirections, writes or outside \
        paths) requires explicit user approval and is denied after 60 seconds without an \
        answer. Keep commands read-only: this is an analysis environment."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        schema_value::<Self::Args>()
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, AgentToolError> {
        let command = args.command.trim().to_string();
        if command.is_empty() {
            return Err(AgentToolError::new("command must not be empty"));
        }
        if let BashClassification::NeedsApproval(reason) =
            classify_bash_command(&command, &self.0.scope.workspace_root)
            && !request_bash_approval(&self.0, &command, &reason, BASH_APPROVAL_TIMEOUT).await?
        {
            return Ok(BashOutput {
                executed: false,
                status: "denied_by_user".to_string(),
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                stdout_truncated: false,
                stderr_truncated: false,
            });
        }
        let timeout_ms = args
            .timeout_ms
            .unwrap_or(DEFAULT_BASH_TIMEOUT_MS)
            .clamp(1_000, MAX_BASH_TIMEOUT_MS);
        let mut process = tokio::process::Command::new("bash");
        process
            .arg("-c")
            .arg(&command)
            .current_dir(&self.0.scope.workspace_root)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            // 会话取消后 future 被丢弃，此时必须连带终止子进程，避免残留后台命令。
            .kill_on_drop(true);
        let output =
            match tokio::time::timeout(Duration::from_millis(timeout_ms), process.output()).await {
                Ok(Ok(output)) => output,
                Ok(Err(error)) => {
                    return Err(AgentToolError::new(format!("cannot spawn bash: {error}")));
                }
                Err(_) => {
                    return Err(AgentToolError::new(format!(
                        "command timed out after {timeout_ms} ms and was terminated"
                    )));
                }
            };
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let stdout_truncated = stdout.len() > MAX_BASH_STDOUT_BYTES;
        let stderr_truncated = stderr.len() > MAX_BASH_STDERR_BYTES;
        Ok(BashOutput {
            executed: true,
            status: String::new(),
            exit_code: Some(output.status.code().unwrap_or(-1)),
            stdout: truncate_utf8_with_ellipsis(
                redact_sensitive_text(&stdout),
                MAX_BASH_STDOUT_BYTES,
            ),
            stderr: truncate_utf8_with_ellipsis(
                redact_sensitive_text(&stderr),
                MAX_BASH_STDERR_BYTES,
            ),
            stdout_truncated,
            stderr_truncated,
        })
    }
}

/// 请求用户审批一条命令；返回是否批准。超时与取消都按拒绝收敛。
pub(crate) async fn request_bash_approval(
    context: &AgentOperationContext,
    command: &str,
    reason: &str,
    timeout: Duration,
) -> Result<bool, AgentToolError> {
    let request_id = Uuid::new_v4().to_string();
    let (decision_sender, decision_receiver) = tokio::sync::oneshot::channel();
    {
        let mut pending = context
            .bash_pending_approvals
            .lock()
            .map_err(|_| AgentToolError::new("approval state corrupted"))?;
        pending.insert(request_id.clone(), decision_sender);
    }
    if context
        .event_sender
        .send(AgentEvent::BashApprovalRequired {
            request_id: request_id.clone(),
            command: command.to_string(),
        })
        .await
        .is_err()
    {
        // 界面通道已关闭，会话即将终止；直接按拒绝收敛。
        context
            .bash_pending_approvals
            .lock()
            .ok()
            .map(|mut pending| pending.remove(&request_id));
        return Ok(false);
    }
    let outcome = tokio::select! {
        decision = decision_receiver => match decision {
            Ok(approved) => (approved, "user_decision"),
            Err(_) => (false, "session_closed"),
        },
        _ = tokio::time::sleep(timeout) => (false, "approval_timeout"),
        _ = context.cancellation.cancelled() => (false, "session_cancelled"),
    };
    context
        .bash_pending_approvals
        .lock()
        .map_err(|_| AgentToolError::new("approval state corrupted"))?
        .remove(&request_id);
    let _ = context
        .event_sender
        .try_send(AgentEvent::BashApprovalOutcome {
            request_id,
            approved: outcome.0,
            reason: format!("{reason}|{}", outcome.1),
        });
    Ok(outcome.0)
}

/// 把所有等待中的审批按拒绝收敛；会话退出时由循环守卫调用。
pub(crate) fn reject_pending_bash_approvals(context: &AgentOperationContext, reason: &str) {
    let Ok(mut pending) = context.bash_pending_approvals.lock() else {
        return;
    };
    for (request_id, sender) in pending.drain() {
        // 发送端丢弃同样会让等待方按拒绝收敛；显式 send false 保持语义一致。
        let _ = sender.send(false);
        let _ = context
            .event_sender
            .try_send(AgentEvent::BashApprovalOutcome {
                request_id,
                approved: false,
                reason: reason.to_string(),
            });
    }
}

/// 判定一条命令能否免确认执行。
///
/// 逐段（管道、`;`、`&&`、`||`、换行）解析：每段的首词必须是白名单只读命令、
/// 不得包含重定向或命令替换等元操作、全部路径形态参数都必须落在工作目录内；
/// 任一条件不满足即转用户确认。
pub(crate) fn classify_bash_command(command: &str, workspace_root: &Path) -> BashClassification {
    for segment in split_command_segments(command) {
        if let Some(reason) = segment_meta_operations(&segment) {
            return BashClassification::NeedsApproval(reason);
        }
        let tokens = tokenize_segment(&segment);
        let mut tokens = tokens.iter().peekable();
        // 跳过段首环境变量赋值前缀（FOO=bar ls）。
        while let Some(token) = tokens.peek() {
            if is_env_assignment(token) {
                tokens.next();
            } else {
                break;
            }
        }
        let Some(command_word) = tokens.next().map(String::as_str) else {
            continue;
        };
        let command_name = Path::new(command_word)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| command_word.to_string());
        if !READONLY_COMMAND_WHITELIST.contains(&command_name.as_str()) {
            return BashClassification::NeedsApproval(format!(
                "command \"{command_name}\" is outside the read-only whitelist"
            ));
        }
        let rest: Vec<&str> = tokens.map(String::as_str).collect();
        if let Some(reason) = destructive_whitelisted_usage(&command_name, &rest) {
            return BashClassification::NeedsApproval(reason);
        }
        if let Some(reason) = out_of_workspace_argument(&rest, workspace_root) {
            return BashClassification::NeedsApproval(reason);
        }
    }
    BashClassification::AutoRun
}

/// 检测段内的重定向与命令替换；引号内的字符不参与判定。
fn segment_meta_operations(segment: &str) -> Option<String> {
    let mut quote: Option<char> = None;
    for character in segment.chars() {
        match quote {
            Some(open) if character == open => quote = None,
            Some(_) => {}
            None => match character {
                '\'' | '"' => quote = Some(character),
                '>' => return Some("output redirection writes files".to_string()),
                '$' | '`' => {
                    return Some("command substitution spawns arbitrary commands".to_string());
                }
                _ => {}
            },
        }
    }
    None
}

/// 把命令拆分为按 `|`、`;`、`&` 和换行分隔的执行段；引号内的分隔符不生效。
fn split_command_segments(command: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for character in command.chars() {
        match quote {
            Some(open) if character == open => {
                quote = None;
                current.push(character);
            }
            Some(_) => current.push(character),
            None => match character {
                '\'' | '"' => {
                    quote = Some(character);
                    current.push(character);
                }
                '|' | ';' | '&' | '\n' | '\r' => {
                    segments.push(std::mem::take(&mut current));
                }
                _ => current.push(character),
            },
        }
    }
    segments.push(current);
    segments
        .into_iter()
        .map(|segment| segment.trim().to_string())
        .filter(|segment| !segment.is_empty())
        .collect()
}

/// 按空白切分单个执行段；引号内空白不切分，引号本身保留给路径检查环节。
fn tokenize_segment(segment: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for character in segment.chars() {
        match quote {
            Some(open) if character == open => {
                quote = None;
                current.push(character);
            }
            Some(_) => current.push(character),
            None => match character {
                '\'' | '"' => {
                    quote = Some(character);
                    current.push(character);
                }
                character if character.is_whitespace() => {
                    if !current.is_empty() {
                        tokens.push(std::mem::take(&mut current));
                    }
                }
                _ => current.push(character),
            },
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// 判定 token 是否为段首环境变量赋值。
fn is_env_assignment(token: &str) -> bool {
    let Some((name, value)) = token.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
        && !value.is_empty()
}

/// 白名单命令仍会改动文件或派生执行的参数形态。
fn destructive_whitelisted_usage(command_name: &str, args: &[&str]) -> Option<String> {
    match command_name {
        "sed" => args
            .iter()
            .find(|arg| **arg == "-i" || arg.starts_with("--in-place"))
            .map(|_| "sed in-place editing modifies files".to_string()),
        "find" => args
            .iter()
            .find(|arg| {
                matches!(**arg, "-delete" | "-exec" | "-execdir" | "-ok" | "-okdir")
                    || arg.starts_with("-fprint")
            })
            .map(|arg| format!("find action \"{arg}\" modifies files or spawns commands")),
        "sort" => args
            .iter()
            .find(|arg| **arg == "-o" || arg.starts_with("--output"))
            .map(|_| "sort --output writes files".to_string()),
        "awk" => args
            .iter()
            .any(|arg| arg.contains("system("))
            .then(|| "awk program spawns subprocesses".to_string()),
        _ => None,
    }
}

/// 检查参数中路径形态的 token 是否全部落在工作目录内。
fn out_of_workspace_argument(args: &[&str], workspace_root: &Path) -> Option<String> {
    for arg in args {
        let looks_like_path = arg.starts_with('/')
            || arg.starts_with("./")
            || arg.starts_with("../")
            || arg.starts_with('~')
            || *arg == "."
            || *arg == ".."
            || arg.contains('/');
        if !looks_like_path {
            continue;
        }
        if arg.starts_with('~') {
            return Some(format!(
                "argument \"{arg}\" expands to the user home directory"
            ));
        }
        let expanded = arg
            .strip_prefix("./")
            .map(|rest| format!("{}/{}", workspace_root.display(), rest))
            .unwrap_or_else(|| arg.to_string());
        let mut resolved = workspace_root.to_path_buf();
        let mut escaped = false;
        for component in Path::new(&expanded).components() {
            match component {
                std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                    resolved = PathBuf::new();
                    resolved.push(component.as_os_str());
                }
                std::path::Component::ParentDir => {
                    if !resolved.pop() {
                        escaped = true;
                        break;
                    }
                }
                std::path::Component::CurDir => {}
                other => resolved.push(other.as_os_str()),
            }
        }
        if escaped || !resolved.starts_with(workspace_root) {
            return Some(format!("argument \"{arg}\" leaves the workspace directory"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::session::{
        AgentBudget, BashApprovalDecision, LogProfileSnapshot, SourceScopeSnapshot,
    };
    use crate::config::paths::temporary_test_dir;
    use async_channel::unbounded;
    use std::collections::HashMap as StdHashMap;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;

    /// 构造带临时工作目录和显式授权开关的测试上下文及其事件接收端。
    fn test_context(
        workspace: &Path,
        allow_raw_log_content: bool,
    ) -> (
        Arc<AgentOperationContext>,
        async_channel::Receiver<AgentEvent>,
    ) {
        let (event_sender, event_receiver) = unbounded::<AgentEvent>();
        let (_decision_sender, decision_receiver) = unbounded::<BashApprovalDecision>();
        (
            Arc::new(AgentOperationContext {
                scope: Arc::new(SourceScopeSnapshot {
                    session_id: "test".to_string(),
                    root_label: "logs".to_string(),
                    workspace_root: workspace.to_path_buf(),
                    preferred_encoding: "UTF-8".to_string(),
                    sources: Arc::new(Vec::new()),
                    profiles: Arc::new(StdHashMap::<String, LogProfileSnapshot>::new()),
                    allow_raw_log_content,
                }),
                budget: Arc::new(AgentBudget::balanced()),
                cancellation: tokio_util::sync::CancellationToken::new(),
                event_sender,
                accepted_user_messages: Mutex::new(Vec::new()),
                pending_user_messages: Arc::new(AtomicUsize::new(0)),
                bash_pending_approvals: Arc::new(Mutex::new(StdHashMap::new())),
                bash_decision_receiver: decision_receiver,
            }),
            event_receiver,
        )
    }

    /// 白名单只读命令直接放行；管道每段都要白名单。
    #[test]
    fn whitelisted_readonly_commands_auto_run() {
        let workspace = temporary_test_dir("bash-classify-allow");
        let root = workspace.path().to_path_buf();
        for command in [
            "ls -la",
            "grep -n ERROR logs/app.log",
            "cat logs/app.log | grep -i oom | sort | uniq -c",
            "head -n 100 logs/app.log",
            "awk '{print $1}' logs/app.log",
            "FOO=1 BAR=2 wc -l logs/app.log",
            "df -h",
            "du -sh logs",
        ] {
            assert_eq!(
                classify_bash_command(command, &root),
                BashClassification::AutoRun,
                "命令应免确认执行：{command}"
            );
        }
    }

    /// 非白名单命令、重定向、越界路径、命令替换与破坏性参数全部转确认。
    #[test]
    fn risky_commands_require_approval() {
        let workspace = temporary_test_dir("bash-classify-confirm");
        let root = workspace.path().to_path_buf();
        for command in [
            "rm -rf logs",
            "ls | xargs rm",
            "grep ERROR logs/app.log > out.txt",
            "cat /etc/passwd",
            "sed -i 's/a/b/' logs/app.log",
            "find . -delete",
            "sort -o out.txt logs/app.log",
            "echo $(rm -rf /)",
            "echo $(rm x)",
            "cat ~/secret.key",
            "cat ../outside.log",
            "ls\nrm -rf /",
            "grep ERROR logs/app.log && rm x",
            "python3 analyze.py",
        ] {
            assert!(
                matches!(
                    classify_bash_command(command, &root),
                    BashClassification::NeedsApproval(_)
                ),
                "命令应转用户确认：{command}"
            );
        }
    }

    /// read_file 拒绝越界路径并正常读取工作目录内文件。
    #[tokio::test]
    async fn read_file_rejects_escape_and_reads_lines() {
        let workspace = temporary_test_dir("read-file-tool");
        let root = workspace.path().to_path_buf();
        std::fs::create_dir_all(root.join("logs")).expect("应创建目录");
        std::fs::write(root.join("logs/app.log"), "one\ntwo\nthree\n").expect("应写入日志");
        let (context, _event_receiver) = test_context(&root, true);

        let tool = ReadFileTool(context);
        for escape in ["../outside.log", "/etc/passwd", "logs/../../escape"] {
            let error = tool
                .call(ReadFileArgs {
                    path: escape.to_string(),
                    offset_line: None,
                    max_lines: None,
                })
                .await
                .expect_err("越界路径必须被拒绝");
            assert!(
                error.to_string().contains("escapes the workspace"),
                "错误应说明越界：{error}"
            );
        }
        let output = tool
            .call(ReadFileArgs {
                path: "logs/app.log".to_string(),
                offset_line: Some(2),
                max_lines: Some(2),
            })
            .await
            .expect("工作目录内文件应可读取");
        assert_eq!(output.path, "logs/app.log");
        assert_eq!(output.line_count, 2);
        assert_eq!(output.content, "two\nthree\n");
    }

    /// read_file 在未授权发送原文时只返回元数据。
    #[tokio::test]
    async fn read_file_withholds_content_without_authorization() {
        let workspace = temporary_test_dir("read-file-withheld");
        let root = workspace.path().to_path_buf();
        std::fs::create_dir_all(root.join("logs")).expect("应创建目录");
        std::fs::write(root.join("logs/app.log"), "secret\n").expect("应写入日志");
        let (context, _event_receiver) = test_context(&root, false);
        let output = ReadFileTool(context)
            .call(ReadFileArgs {
                path: "logs/app.log".to_string(),
                offset_line: None,
                max_lines: None,
            })
            .await
            .expect("元数据返回不应失败");
        assert!(output.content.is_empty());
        assert!(output.content_withheld.is_some());
    }

    /// bash 工具实际执行白名单命令并回传退出码与输出。
    #[tokio::test]
    async fn bash_tool_runs_whitelisted_command_in_workspace() {
        let workspace = temporary_test_dir("bash-tool-run");
        let root = workspace.path().to_path_buf();
        std::fs::write(root.join("app.log"), "alpha\nbeta\n").expect("应写入日志");
        let (context, _event_receiver) = test_context(&root, true);
        let output = BashTool(context)
            .call(BashArgs {
                command: "cat app.log | wc -l".to_string(),
                timeout_ms: None,
            })
            .await
            .expect("白名单命令应执行");
        assert!(output.executed);
        assert_eq!(output.exit_code, Some(0));
        assert_eq!(output.stdout.trim(), "2");
    }

    /// 审批超时按拒绝收敛，且先后发布审批请求与结论事件。
    #[tokio::test]
    async fn bash_approval_times_out_to_denial() {
        let workspace = temporary_test_dir("bash-approval-timeout");
        let root = workspace.path().to_path_buf();
        let (context, event_receiver) = test_context(&root, true);
        let approved = request_bash_approval(
            &context,
            "rm -rf .",
            "outside whitelist",
            Duration::from_millis(50),
        )
        .await
        .expect("审批流程不应出错");
        assert!(!approved);
        let mut required = false;
        let mut outcome = None;
        while let Ok(event) = event_receiver.try_recv() {
            match event {
                AgentEvent::BashApprovalRequired { .. } => required = true,
                AgentEvent::BashApprovalOutcome { reason, .. } => outcome = Some(reason),
                _ => {}
            }
        }
        assert!(required, "应发布审批请求事件");
        assert!(
            outcome
                .expect("应发布审批结论事件")
                .contains("approval_timeout"),
            "结论应说明超时"
        );
        assert!(
            context
                .bash_pending_approvals
                .lock()
                .expect("审批状态应可读")
                .is_empty()
        );
    }

    /// 会话退出守卫把等待中的审批按拒绝收敛。
    #[tokio::test]
    async fn pending_approvals_rejected_on_session_exit() {
        let workspace = temporary_test_dir("bash-approval-exit");
        let root = workspace.path().to_path_buf();
        let (context, _event_receiver) = test_context(&root, true);
        let (sender, receiver) = tokio::sync::oneshot::channel();
        context
            .bash_pending_approvals
            .lock()
            .expect("审批状态应可写")
            .insert("req-1".to_string(), sender);
        reject_pending_bash_approvals(&context, "session_ended");
        assert!(
            !receiver.await.expect("守卫应送达拒绝结论"),
            "等待中的审批必须按拒绝收敛"
        );
    }

    /// 敏感字段和 Bearer Token 在进入模型上下文前会被遮蔽。
    #[test]
    fn redaction_masks_common_secrets() {
        let value = redact_sensitive_text("password=hunter2 Authorization: Bearer abc.def");
        assert!(value.contains("[REDACTED]"));
        assert!(!value.contains("hunter2"));
        assert!(!value.contains("abc.def"));
    }
}
