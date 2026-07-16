//! 文件职责：定义 AI 日志分析能力的非敏感配置、日志类型说明和名称匹配规则。
//! 创建日期：2026-07-15
//! 修改日期：2026-07-16
//! 作者：Argus 开发团队
//! 主要功能：提供 OpenAI 兼容端点、专业系统提示词、预算档位、自定义日志类型匹配、校验及规范化逻辑。

use regex::{Regex, RegexBuilder};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use url::Url;

/// AI 设置允许保存的日志类型配置上限，避免配置异常膨胀。
pub(crate) const MAX_LOG_PROFILE_COUNT: usize = 100;
/// AI 设置允许保存的模型配置上限，避免误操作导致设置文件异常膨胀。
pub(crate) const MAX_AI_MODEL_PROFILE_COUNT: usize = 20;
/// 新增模型默认上下文窗口大小，兼顾常见云端和本地工具调用模型。
pub(crate) const DEFAULT_AI_CONTEXT_WINDOW_TOKENS: u64 = 128_000;
/// 允许配置的最小上下文窗口，过小无法稳定容纳系统提示、工具 Schema 和日志证据。
pub(crate) const MIN_AI_CONTEXT_WINDOW_TOKENS: u64 = 4_096;
/// 允许配置的最大上下文窗口，阻止异常配置导致比例计算失真。
pub(crate) const MAX_AI_CONTEXT_WINDOW_TOKENS: u64 = 10_000_000;
/// 当前原文授权说明版本；版本变化后用户需要重新确认。
pub(crate) const AI_RAW_LOG_CONSENT_VERSION: &str = "2026-07-15";
/// 用户可编辑系统提示词的最大 UTF-8 字节数，避免设置文件和每轮模型上下文异常膨胀。
pub(crate) const MAX_AI_SYSTEM_PROMPT_BYTES: usize = 32 * 1024;
/// 新安装和旧配置迁移时使用的专业分析提示词。
///
/// 该提示词只负责角色、分析质量和表达要求；工具权限、证据校验、固定分析流程等安全规则
/// 由编排器另行注入，用户修改此字段不能覆盖那些强制规则。
pub(crate) const DEFAULT_AI_SYSTEM_PROMPT: &str = r#"你是一名资深生产故障分析与日志取证专家。请以可复核、可证伪的方式工作，优先建立事实，再提出假设。

分析要求：
- 明确区分直接观察、合理推断和未知信息，不把相关性表述为因果关系。
- 主动检查时间范围、时区、日志缺口、重复事件、采样偏差以及跨组件关联 ID。
- 对每个候选根因同时寻找支持证据和反证；存在冲突时说明冲突及其影响。
- 优先使用确定性统计、聚合和专用分析器缩小范围，再读取最少量的必要上下文。
- 建议应具体、可执行并包含验证方法；无法确认时明确还需要哪些数据。
- 使用准确、简洁的中文，不夸大置信度，不隐藏限制。"#;
/// 进程内日志名称正则缓存上限，防止长时间编辑配置导致缓存无限增长。
const LOG_MATCHER_REGEX_CACHE_LIMIT: usize = 2048;
/// 已校验日志名称正则的共享缓存；完整来源扫描和命中统计复用同一编译结果。
static LOG_MATCHER_REGEX_CACHE: OnceLock<Mutex<HashMap<(String, bool), Regex>>> = OnceLock::new();

/// AI 日志分析非敏感配置；API Key 始终存放在系统凭据库中。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct AiConfig {
    /// 旧版本单模型 API 根地址；仅用于向多模型配置迁移，不再由新界面写入。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub base_url: String,
    /// 旧版本单模型标识；仅用于向多模型配置迁移，不再由新界面写入。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// 可供每次智能分析会话选择的模型配置。
    #[serde(default)]
    pub model_profiles: Vec<AiModelProfile>,
    /// 是否允许把经工具裁剪的必要日志原文发送给模型。
    #[serde(default)]
    pub allow_raw_log_content: bool,
    /// 用户确认过的原文授权说明版本。
    #[serde(default)]
    pub consent_version: String,
    /// 当前会话资源预算档位。
    #[serde(default)]
    pub budget_profile: AiBudgetProfile,
    /// 单次模型请求超时秒数。
    #[serde(default = "default_request_timeout_seconds")]
    pub request_timeout_seconds: u64,
    /// 用户可编辑的专业分析系统提示词；不可覆盖编排器内置的权限、证据和流程规则。
    #[serde(default = "default_ai_system_prompt")]
    pub system_prompt: String,
    /// 用户定义的日志类型和分析说明。
    #[serde(default)]
    pub log_profiles: Vec<LogTypeProfile>,
}

impl AiConfig {
    /// 原地规范化可安全修正的 AI 配置字段。
    ///
    /// 该方法不会擅自修复无效 URL 或正则；这些错误由 `validate` 明确反馈给用户。
    pub(crate) fn normalize(&mut self) {
        self.base_url = self.base_url.trim().trim_end_matches('/').to_string();
        if !self.base_url.is_empty() && !self.base_url.ends_with("/v1") {
            self.base_url.push_str("/v1");
        }
        self.model = self.model.trim().to_string();
        // 旧版本只有一组地址和模型 ID；首次读取时迁移为具备稳定 ID 的模型配置。
        if self.model_profiles.is_empty() && (!self.base_url.is_empty() || !self.model.is_empty()) {
            self.model_profiles.push(AiModelProfile {
                profile_id: uuid::Uuid::new_v4().to_string(),
                enabled: true,
                name: if self.model.is_empty() {
                    "默认模型".to_string()
                } else {
                    self.model.clone()
                },
                base_url: self.base_url.clone(),
                model: self.model.clone(),
                context_window_tokens: DEFAULT_AI_CONTEXT_WINDOW_TOKENS,
            });
        }
        self.base_url.clear();
        self.model.clear();
        self.request_timeout_seconds = self.request_timeout_seconds.clamp(10, 600);
        self.system_prompt = self.system_prompt.trim().to_string();
        if self.system_prompt.is_empty() {
            self.system_prompt = default_ai_system_prompt();
        }
        self.model_profiles.truncate(MAX_AI_MODEL_PROFILE_COUNT);
        for profile in &mut self.model_profiles {
            profile.normalize();
        }
        self.log_profiles.truncate(MAX_LOG_PROFILE_COUNT);
        for profile in &mut self.log_profiles {
            profile.normalize();
        }
    }

    /// 校验模型端点、授权和全部日志类型配置。
    ///
    /// 返回值：配置可用于新会话时返回 `Ok`；否则返回第一条用户可读错误。
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.allow_raw_log_content && self.consent_version != AI_RAW_LOG_CONSENT_VERSION {
            return Err("日志原文授权说明已更新，请在设置中重新确认".to_string());
        }
        self.validate_model_profiles()?;
        if !self.model_profiles.iter().any(|profile| profile.enabled) {
            return Err("请至少启用一个可用于智能分析的模型".to_string());
        }
        self.validate_log_profiles()?;
        self.validate_system_prompt()?;
        Ok(())
    }

    /// 校验用户可编辑的专业提示词，控制持久化大小并拒绝不可见控制字符。
    pub(crate) fn validate_system_prompt(&self) -> Result<(), String> {
        if self.system_prompt.trim().is_empty() {
            return Err("默认系统提示词不能为空".to_string());
        }
        if self.system_prompt.len() > MAX_AI_SYSTEM_PROMPT_BYTES {
            return Err(format!(
                "默认系统提示词不能超过 {} KiB",
                MAX_AI_SYSTEM_PROMPT_BYTES / 1024
            ));
        }
        if self
            .system_prompt
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        {
            return Err("默认系统提示词包含不支持的控制字符".to_string());
        }
        Ok(())
    }

    /// 校验全部日志类型配置和稳定 ID；即使尚未配置模型，设置保存也不能持久化歧义配置。
    pub(crate) fn validate_log_profiles(&self) -> Result<(), String> {
        let mut profile_ids = HashSet::new();
        for profile in &self.log_profiles {
            profile.validate()?;
            if !profile_ids.insert(profile.profile_id.as_str()) {
                return Err(format!("日志类型“{}”使用了重复的 profile_id", profile.name));
            }
        }
        Ok(())
    }

    /// 校验全部已保存模型配置和稳定 ID，供保存设置及启动预检复用。
    pub(crate) fn validate_model_profiles(&self) -> Result<(), String> {
        if self.model_profiles.is_empty() {
            return Err("尚未配置模型，请先在设置中新增模型".to_string());
        }
        let mut profile_ids = HashSet::new();
        for profile in &self.model_profiles {
            profile.validate()?;
            if !profile_ids.insert(profile.profile_id.as_str()) {
                return Err(format!("模型“{}”使用了重复的 profile_id", profile.name));
            }
        }
        Ok(())
    }

    /// 按稳定 ID 返回已启用模型；会话必须显式选择模型，避免编辑顺序变化后误用其它服务。
    pub(crate) fn enabled_model(&self, profile_id: &str) -> Result<&AiModelProfile, String> {
        let profile = self
            .model_profiles
            .iter()
            .find(|profile| profile.profile_id == profile_id)
            .ok_or_else(|| "所选模型配置已不存在，请重新选择".to_string())?;
        if !profile.enabled {
            return Err(format!("模型“{}”当前已停用", profile.name));
        }
        profile.validate()?;
        Ok(profile)
    }
}

impl Default for AiConfig {
    /// 构造默认未配置模型的 AI 配置；没有可用模型时不会发起任何网络请求。
    fn default() -> Self {
        Self {
            base_url: String::new(),
            model: String::new(),
            model_profiles: Vec::new(),
            allow_raw_log_content: false,
            consent_version: String::new(),
            budget_profile: AiBudgetProfile::Balanced,
            request_timeout_seconds: default_request_timeout_seconds(),
            system_prompt: default_ai_system_prompt(),
            log_profiles: Vec::new(),
        }
    }
}

/// 返回默认系统提示词的独立所有权副本，供 Serde 默认值、配置迁移和界面重置复用。
fn default_ai_system_prompt() -> String {
    DEFAULT_AI_SYSTEM_PROMPT.to_string()
}

/// 一项可独立启停和选择的 OpenAI 兼容模型配置。
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub(crate) struct AiModelProfile {
    /// 稳定 UUID；编辑配置时保持不变，并由启动对话框传入会话。
    pub profile_id: String,
    /// 是否允许新会话选择该模型。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 用户可读名称，用于设置列表和会话模型选择。
    pub name: String,
    /// OpenAI 兼容 API 根地址，规范化后以 `/v1` 结尾。
    pub base_url: String,
    /// 服务端模型标识。
    pub model: String,
    /// 模型可接受的上下文窗口 Token 数，用于分析窗口展示当前请求占用比例。
    #[serde(default = "default_context_window_tokens")]
    pub context_window_tokens: u64,
}

impl AiModelProfile {
    /// 创建一条用于新增对话框的空白模型配置。
    pub(crate) fn new() -> Self {
        Self {
            profile_id: uuid::Uuid::new_v4().to_string(),
            enabled: true,
            name: String::new(),
            base_url: String::new(),
            model: String::new(),
            context_window_tokens: DEFAULT_AI_CONTEXT_WINDOW_TOKENS,
        }
    }

    /// 规范化名称、地址和模型标识两端空白，并补齐兼容 API 的 `/v1` 路径。
    pub(crate) fn normalize(&mut self) {
        self.profile_id = self.profile_id.trim().to_string();
        self.name = self.name.trim().to_string();
        self.base_url = self.base_url.trim().trim_end_matches('/').to_string();
        if !self.base_url.is_empty() && !self.base_url.ends_with("/v1") {
            self.base_url.push_str("/v1");
        }
        self.model = self.model.trim().to_string();
    }

    /// 校验一条模型配置能否被保存和用于新会话。
    pub(crate) fn validate(&self) -> Result<(), String> {
        if uuid::Uuid::parse_str(&self.profile_id).is_err() {
            return Err(format!("模型“{}”的 profile_id 不是有效 UUID", self.name));
        }
        if !(1..=64).contains(&self.name.chars().count()) {
            return Err("模型名称长度必须为 1～64 个字符".to_string());
        }
        let url = validate_ai_base_url(&self.base_url)?;
        if self.model.trim().is_empty() {
            return Err(format!("模型“{}”的模型 ID 不能为空", self.name));
        }
        if !(MIN_AI_CONTEXT_WINDOW_TOKENS..=MAX_AI_CONTEXT_WINDOW_TOKENS)
            .contains(&self.context_window_tokens)
        {
            return Err(format!(
                "模型“{}”的上下文大小必须为 4,096～10,000,000 Token",
                self.name
            ));
        }
        // 使用解析结果，明确 URL 已完成结构校验且不是单纯依赖副作用。
        let _validated_scheme = url.scheme();
        Ok(())
    }

    /// 返回适合紧凑设置列表展示的上下文窗口容量。
    pub(crate) fn context_window_label(&self) -> String {
        if self.context_window_tokens >= 1_000_000 {
            format!(
                "{:.1}M Token",
                self.context_window_tokens as f64 / 1_000_000.0
            )
        } else if self.context_window_tokens >= 1_000 {
            format!("{:.0}K Token", self.context_window_tokens as f64 / 1_000.0)
        } else {
            format!("{} Token", self.context_window_tokens)
        }
    }
}

/// 首期资源预算档位；保留枚举以便后续增加保守或深度分析档。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AiBudgetProfile {
    /// 平衡延迟、费用和分析深度的默认档位。
    #[default]
    Balanced,
}

/// 用户定义的日志类型和发送给模型的分析说明。
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub(crate) struct LogTypeProfile {
    /// 稳定 UUID；编辑配置时保持不变。
    pub profile_id: String,
    /// 是否参与新会话匹配。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 用户可读日志类型名称。
    pub name: String,
    /// 数值越大匹配优先级越高。
    #[serde(default = "default_profile_priority")]
    pub priority: u16,
    /// 任意一个命中即视为配置命中。
    #[serde(default)]
    pub matchers: Vec<LogNameMatcher>,
    /// 发送给模型的业务分析说明。
    pub description: String,
}

impl LogTypeProfile {
    /// 规范化用户可读字段和匹配模式两端空白。
    fn normalize(&mut self) {
        self.profile_id = self.profile_id.trim().to_string();
        self.name = self.name.trim().to_string();
        self.description = self.description.trim().to_string();
        self.priority = self.priority.min(1000);
        self.matchers.truncate(16);
        for matcher in &mut self.matchers {
            matcher.pattern = matcher.pattern.trim().to_string();
        }
    }

    /// 校验一条日志类型配置是否可进入会话快照。
    pub(crate) fn validate(&self) -> Result<(), String> {
        if uuid::Uuid::parse_str(&self.profile_id).is_err() {
            return Err(format!(
                "日志类型“{}”的 profile_id 不是有效 UUID",
                self.name
            ));
        }
        if !(1..=64).contains(&self.name.chars().count()) {
            return Err("日志类型名称长度必须为 1～64 个字符".to_string());
        }
        if !(1..=4096).contains(&self.description.len()) {
            return Err(format!("日志类型“{}”的说明必须为 1 B～4 KiB", self.name));
        }
        if !(1..=16).contains(&self.matchers.len()) {
            return Err(format!("日志类型“{}”必须配置 1～16 个名称规则", self.name));
        }
        for matcher in &self.matchers {
            matcher.validate()?;
        }
        Ok(())
    }
}

/// 名称匹配器选择的目标字段。
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LogNameMatcherTarget {
    /// 只匹配末级文件名。
    FileName,
    /// 匹配来源根内统一使用 `/` 的相对展示路径。
    RelativePath,
}

/// 名称规则的匹配方式。
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LogNameMatcherMode {
    /// 整体相等。
    Exact,
    /// 以前缀开始。
    Prefix,
    /// 以后缀结束。
    Suffix,
    /// 包含指定文本。
    Contains,
    /// 使用项目现有 Rust regex 语法。
    Regex,
}

/// 一条可校验的日志名称匹配规则。
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub(crate) struct LogNameMatcher {
    /// 规则作用的名称字段。
    pub target: LogNameMatcherTarget,
    /// 匹配算法。
    pub mode: LogNameMatcherMode,
    /// 文本或正则模式。
    pub pattern: String,
    /// 是否区分大小写。
    #[serde(default)]
    pub case_sensitive: bool,
}

impl LogNameMatcher {
    /// 校验规则长度并提前编译正则，阻止带病配置进入会话。
    pub(crate) fn validate(&self) -> Result<(), String> {
        if !(1..=512).contains(&self.pattern.chars().count()) {
            return Err("日志名称匹配模式长度必须为 1～512 个字符".to_string());
        }
        if self.mode == LogNameMatcherMode::Regex {
            cached_log_matcher_regex(&self.pattern, self.case_sensitive)
                .map_err(|error| format!("日志名称正则无效：{error}"))?;
        }
        Ok(())
    }

    /// 判断文件名和相对路径是否命中当前规则。
    pub(crate) fn is_match(&self, file_name: &str, relative_path: &str) -> bool {
        let value = match self.target {
            LogNameMatcherTarget::FileName => file_name,
            LogNameMatcherTarget::RelativePath => relative_path,
        };
        if self.mode == LogNameMatcherMode::Regex {
            return cached_log_matcher_regex(&self.pattern, self.case_sensitive)
                .is_ok_and(|regex| regex.is_match(value));
        }
        let (value, pattern) = if self.case_sensitive {
            (value.to_string(), self.pattern.clone())
        } else {
            (value.to_lowercase(), self.pattern.to_lowercase())
        };
        match self.mode {
            LogNameMatcherMode::Exact => value == pattern,
            LogNameMatcherMode::Prefix => value.starts_with(&pattern),
            LogNameMatcherMode::Suffix => value.ends_with(&pattern),
            LogNameMatcherMode::Contains => value.contains(&pattern),
            LogNameMatcherMode::Regex => false,
        }
    }
}

/// 返回已编译的日志名称正则；同一模式和大小写配置在进程内只编译一次。
fn cached_log_matcher_regex(pattern: &str, case_sensitive: bool) -> Result<Regex, regex::Error> {
    let key = (pattern.to_string(), case_sensitive);
    let cache = LOG_MATCHER_REGEX_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(cache) = cache.lock()
        && let Some(regex) = cache.get(&key)
    {
        return Ok(regex.clone());
    }

    let regex = RegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .build()?;
    if let Ok(mut cache) = cache.lock() {
        if cache.len() >= LOG_MATCHER_REGEX_CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(key, regex.clone());
    }
    Ok(regex)
}

/// 解析并验证 AI 服务地址，非回环 HTTP 地址一律拒绝。
pub(crate) fn validate_ai_base_url(value: &str) -> Result<Url, String> {
    let url = Url::parse(value.trim()).map_err(|error| format!("AI 服务地址无效：{error}"))?;
    let host = url.host_str().unwrap_or_default();
    if host.is_empty() {
        return Err("AI 服务地址必须包含主机名".to_string());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("AI 服务地址不能包含用户名或密码，请使用系统凭据库保存 API Key".to_string());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("AI 服务地址不能包含查询参数或片段".to_string());
    }
    let is_loopback = matches!(host, "localhost" | "127.0.0.1" | "::1");
    if url.scheme() != "https" && !(url.scheme() == "http" && is_loopback) {
        return Err("非本机 AI 服务必须使用 HTTPS".to_string());
    }
    if !url.path().trim_end_matches('/').ends_with("/v1") {
        return Err("AI 服务地址必须规范化到 /v1".to_string());
    }
    Ok(url)
}

/// 返回默认模型请求超时秒数。
fn default_request_timeout_seconds() -> u64 {
    120
}

/// 返回模型上下文窗口的兼容默认值，供旧配置反序列化和新增模型复用。
fn default_context_window_tokens() -> u64 {
    DEFAULT_AI_CONTEXT_WINDOW_TOKENS
}

/// 返回日志类型默认优先级。
fn default_profile_priority() -> u16 {
    100
}

/// serde 默认启用值。
fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造可通过安全校验的模型配置，供多模型行为测试复用。
    fn test_model(name: &str) -> AiModelProfile {
        AiModelProfile {
            profile_id: uuid::Uuid::new_v4().to_string(),
            enabled: true,
            name: name.to_string(),
            base_url: "https://example.com/v1".to_string(),
            model: "tool-model".to_string(),
            context_window_tokens: DEFAULT_AI_CONTEXT_WINDOW_TOKENS,
        }
    }

    /// 验证 HTTP 只允许回环地址，避免配置把原文发送到明文远程端点。
    #[test]
    fn ai_url_rejects_remote_http() {
        assert!(validate_ai_base_url("http://example.com/v1").is_err());
        assert!(validate_ai_base_url("http://127.0.0.1:11434/v1").is_ok());
    }

    /// 验证名称匹配支持大小写不敏感的相对路径包含规则。
    #[test]
    fn profile_matcher_matches_relative_path_case_insensitively() {
        let matcher = LogNameMatcher {
            target: LogNameMatcherTarget::RelativePath,
            mode: LogNameMatcherMode::Contains,
            pattern: "Payment".to_string(),
            case_sensitive: false,
        };
        assert!(matcher.is_match("access.log", "gateway/payment/access.log"));
    }

    /// 验证服务地址不能把认证信息混入将持久化的 URL。
    #[test]
    fn ai_url_rejects_embedded_credentials() {
        assert!(validate_ai_base_url("https://user:secret@example.com/v1").is_err());
        assert!(validate_ai_base_url("https://example.com/v1?api_key=secret").is_err());
    }

    /// 验证重复稳定 ID 会被整体拒绝，避免报告把说明版本关联到错误配置。
    #[test]
    fn ai_config_rejects_duplicate_profile_ids() {
        let profile_id = uuid::Uuid::new_v4().to_string();
        let profile = LogTypeProfile {
            profile_id,
            enabled: true,
            name: "应用日志".to_string(),
            priority: 100,
            matchers: vec![LogNameMatcher {
                target: LogNameMatcherTarget::FileName,
                mode: LogNameMatcherMode::Suffix,
                pattern: ".log".to_string(),
                case_sensitive: false,
            }],
            description: "应用日志分析说明".to_string(),
        };
        let mut duplicate = profile.clone();
        duplicate.name = "重复日志".to_string();
        let config = AiConfig {
            model_profiles: vec![test_model("测试模型")],
            log_profiles: vec![profile, duplicate],
            ..AiConfig::default()
        };
        assert!(config.validate().is_err());
    }

    /// 验证旧版单模型字段会在规范化时迁移为具有稳定 ID 的多模型列表。
    #[test]
    fn ai_config_migrates_legacy_single_model() {
        let mut config = AiConfig {
            base_url: "https://example.com/".to_string(),
            model: " legacy-tool-model ".to_string(),
            ..AiConfig::default()
        };
        config.normalize();
        assert!(config.base_url.is_empty());
        assert!(config.model.is_empty());
        assert_eq!(config.model_profiles.len(), 1);
        assert_eq!(config.model_profiles[0].name, "legacy-tool-model");
        assert_eq!(config.model_profiles[0].base_url, "https://example.com/v1");
        assert_eq!(
            config.model_profiles[0].context_window_tokens,
            DEFAULT_AI_CONTEXT_WINDOW_TOKENS
        );
        assert!(uuid::Uuid::parse_str(&config.model_profiles[0].profile_id).is_ok());
    }

    /// 验证上下文窗口必须处于可用于工具分析的合理范围。
    #[test]
    fn ai_model_context_window_must_be_within_supported_range() {
        let mut model = test_model("上下文模型");
        model.context_window_tokens = MIN_AI_CONTEXT_WINDOW_TOKENS - 1;
        assert!(model.validate().is_err());
        model.context_window_tokens = MAX_AI_CONTEXT_WINDOW_TOKENS;
        assert!(model.validate().is_ok());
    }

    /// 验证升级前保存的多模型配置缺少新字段时使用兼容默认上下文窗口。
    #[test]
    fn legacy_model_profile_defaults_context_window() {
        let profile: AiModelProfile = serde_json::from_value(serde_json::json!({
            "profile_id": uuid::Uuid::new_v4().to_string(),
            "enabled": true,
            "name": "旧配置模型",
            "base_url": "https://example.com/v1",
            "model": "legacy-model"
        }))
        .expect("旧模型配置应可反序列化");
        assert_eq!(
            profile.context_window_tokens,
            DEFAULT_AI_CONTEXT_WINDOW_TOKENS
        );
    }

    /// 验证会话按稳定 ID 选择模型，并拒绝列表中已停用的配置。
    #[test]
    fn ai_config_selects_only_enabled_model_by_profile_id() {
        let first = test_model("模型一");
        let mut disabled = test_model("模型二");
        disabled.enabled = false;
        let config = AiConfig {
            model_profiles: vec![first.clone(), disabled.clone()],
            ..AiConfig::default()
        };
        assert_eq!(
            config
                .enabled_model(&first.profile_id)
                .expect("启用模型应可选择")
                .name,
            "模型一"
        );
        assert!(config.enabled_model(&disabled.profile_id).is_err());
    }

    /// 验证模型稳定 ID 不能重复，避免启动对话框选择到歧义配置。
    #[test]
    fn ai_config_rejects_duplicate_model_profile_ids() {
        let first = test_model("模型一");
        let mut duplicate = test_model("模型二");
        duplicate.profile_id = first.profile_id.clone();
        let config = AiConfig {
            model_profiles: vec![first, duplicate],
            ..AiConfig::default()
        };
        assert!(config.validate_model_profiles().is_err());
    }

    /// 验证旧配置缺少系统提示词时自动迁移到专业默认值。
    #[test]
    fn legacy_ai_config_uses_default_system_prompt() {
        let config: AiConfig = serde_json::from_value(serde_json::json!({}))
            .expect("缺少新字段的旧 AI 配置应可反序列化");
        assert_eq!(config.system_prompt, DEFAULT_AI_SYSTEM_PROMPT);
    }

    /// 验证旧版全局关闭值不再覆盖模型可用性，重新保存后也不会继续写出废弃字段。
    #[test]
    fn legacy_global_enabled_flag_is_ignored_after_model_configuration() {
        let config: AiConfig = serde_json::from_value(serde_json::json!({
            "enabled": false,
            "model_profiles": [test_model("已配置模型")]
        }))
        .expect("带旧版全局开关的配置应继续兼容读取");
        config.validate().expect("有效模型配置应直接启用智能分析");
        let serialized = serde_json::to_value(config).expect("AI 配置应可重新序列化");
        assert!(serialized.get("enabled").is_none());
    }

    /// 验证空提示词会在规范化时恢复默认，超大提示词则被保存校验拒绝。
    #[test]
    fn ai_system_prompt_is_normalized_and_bounded() {
        let mut config = AiConfig {
            system_prompt: "  \n".to_string(),
            ..AiConfig::default()
        };
        config.normalize();
        assert_eq!(config.system_prompt, DEFAULT_AI_SYSTEM_PROMPT);
        config.system_prompt = "x".repeat(MAX_AI_SYSTEM_PROMPT_BYTES + 1);
        assert!(config.validate_system_prompt().is_err());
    }
}
