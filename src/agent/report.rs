//! 文件职责：定义并持久化 AI 日志分析的结构化诊断报告。
//! 创建日期：2026-07-15
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：校验报告字段、保存证据引用和使用过的日志说明，并以原子替换方式写入报告目录。

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 默认最多保留的结构化 AI 报告数量。
const REPORT_RETENTION_COUNT: usize = 100;

/// 一条可由用户回到来源行复核的证据引用。
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub(crate) struct EvidenceReference {
    /// 会话范围内的不透明来源引用。
    pub source_ref: String,
    /// 1 基起始行号。
    pub start_line: usize,
    /// 1 基结束行号。
    pub end_line: usize,
    /// 证据支持当前结论的简短说明，不保存日志原文。
    pub rationale: String,
}

/// 一条结构化问题发现。
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub(crate) struct DiagnosticFinding {
    /// 问题标题。
    pub title: String,
    /// 严重度：critical、high、medium、low 或 info。
    pub severity: String,
    /// 结论状态；确认项必须有证据，假设项必须有验证步骤。
    pub status: DiagnosticFindingStatus,
    /// 基于证据的判断说明。
    pub analysis: String,
    /// 对系统或用户的潜在影响。
    pub impact: String,
    /// 建议的后续人工排查或处置动作。
    pub recommendation: String,
    /// 0～1 的模型置信度。
    pub confidence: f32,
    /// 支撑当前发现的证据引用。
    #[serde(default)]
    pub evidence: Vec<EvidenceReference>,
    /// 假设项需要执行的后续验证步骤；确认项可为空。
    #[serde(default)]
    pub verification_steps: Vec<String>,
}

/// 诊断发现的证据成熟度。
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DiagnosticFindingStatus {
    /// 已经由当前来源快照中的日志行证据确认。
    Confirmed,
    /// 当前证据不足，仅作为待验证假设。
    Hypothesis,
}

impl DiagnosticFindingStatus {
    /// 返回独立窗口使用的中文状态标签。
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Confirmed => "已确认",
            Self::Hypothesis => "待验证",
        }
    }
}

/// 报告中记录的日志分析说明版本，不复制完整说明正文。
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub(crate) struct UsedLogProfileSummary {
    /// 稳定日志配置 ID。
    pub profile_id: String,
    /// 用户可读类型名称。
    pub name: String,
    /// 会话快照中的说明 SHA-256 摘要。
    pub description_sha256: String,
}

/// Agent 会话最终提交的结构化诊断报告。
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub(crate) struct DiagnosticReport {
    /// 随机会话标识。
    pub session_id: String,
    /// 用户原始问题的 SHA-256 摘要，用于关联请求但不能恢复正文。
    pub question_sha256: String,
    /// 面向用户的分析结论摘要。
    pub summary: String,
    /// 按重要程度组织的问题发现。
    #[serde(default)]
    pub findings: Vec<DiagnosticFinding>,
    /// 实际使用过的自定义日志类型名称。
    #[serde(default)]
    pub used_log_profiles: Vec<UsedLogProfileSummary>,
    /// 分析局限、未读取范围或需要人工确认的事项。
    #[serde(default)]
    pub limitations: Vec<String>,
    /// RFC3339 格式报告完成时间。
    pub completed_at: String,
}

impl DiagnosticReport {
    /// 对报告执行不依赖模型的边界校验。
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.summary.trim().is_empty() {
            return Err("诊断报告摘要不能为空".to_string());
        }
        if self.summary.len() > 64 * 1024 {
            return Err("诊断报告摘要不能超过 64 KiB".to_string());
        }
        if self.findings.len() > 100 {
            return Err("诊断报告问题数量超过 100 条上限".to_string());
        }
        for finding in &self.findings {
            if finding.title.trim().is_empty()
                || finding.analysis.trim().is_empty()
                || finding.impact.trim().is_empty()
                || finding.recommendation.trim().is_empty()
            {
                return Err("诊断发现的标题、分析、影响和建议不能为空".to_string());
            }
            if !(0.0..=1.0).contains(&finding.confidence) {
                return Err(format!("诊断发现“{}”的置信度必须在 0～1", finding.title));
            }
            if !matches!(
                finding.severity.as_str(),
                "critical" | "high" | "medium" | "low" | "info"
            ) {
                return Err(format!("诊断发现“{}”的严重度无效", finding.title));
            }
            if finding.title.len() > 256
                || finding.analysis.len() > 32 * 1024
                || finding.impact.len() > 8 * 1024
                || finding.recommendation.len() > 8 * 1024
            {
                return Err(format!("诊断发现“{}”的文本超过持久化上限", finding.title));
            }
            match finding.status {
                DiagnosticFindingStatus::Confirmed if finding.evidence.is_empty() => {
                    return Err(format!("已确认发现“{}”必须至少包含一条证据", finding.title));
                }
                DiagnosticFindingStatus::Hypothesis if finding.verification_steps.is_empty() => {
                    return Err(format!("假设发现“{}”必须包含验证步骤", finding.title));
                }
                _ => {}
            }
            for evidence in &finding.evidence {
                if evidence.start_line == 0 || evidence.end_line < evidence.start_line {
                    return Err("诊断证据行号必须为有效的 1 基闭区间".to_string());
                }
                if evidence.rationale.len() > 4096 {
                    return Err("诊断证据说明不能超过 4 KiB".to_string());
                }
            }
        }
        if self.used_log_profiles.len() > 100 || self.limitations.len() > 100 {
            return Err("诊断报告配置摘要或限制说明数量超过 100 条".to_string());
        }
        if self.question_sha256.len() != 64
            || !self
                .question_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("诊断报告问题摘要必须为 SHA-256 十六进制文本".to_string());
        }
        Ok(())
    }
}

/// 计算初始问题摘要，报告持久化只保存该不可逆关联值。
pub(crate) fn question_sha256(question: &str) -> String {
    hex::encode(Sha256::digest(question.as_bytes()))
}

/// 将结构化报告以 JSON 原子写入指定配置根下的 `ai/reports` 目录。
///
/// 返回值：成功时返回最终报告路径；失败时保留内存报告并返回错误。
pub(crate) fn persist_report(
    config_root: &Path,
    report: &DiagnosticReport,
) -> Result<PathBuf, String> {
    report.validate()?;
    let report_dir = config_root.join("ai").join("reports");
    fs::create_dir_all(&report_dir).map_err(|error| format!("创建 AI 报告目录失败：{error}"))?;
    let final_path = report_dir.join(format!("{}.json", report.session_id));
    let mut temporary = tempfile::NamedTempFile::new_in(&report_dir)
        .map_err(|error| format!("创建 AI 报告临时文件失败：{error}"))?;
    let bytes = serde_json::to_vec_pretty(report)
        .map_err(|error| format!("序列化 AI 报告失败：{error}"))?;
    temporary
        .write_all(&bytes)
        .and_then(|_| temporary.flush())
        .map_err(|error| format!("写入 AI 报告失败：{error}"))?;
    temporary
        .persist(&final_path)
        .map_err(|error| format!("提交 AI 报告失败：{}", error.error))?;
    prune_old_reports(&report_dir)?;
    Ok(final_path)
}

/// 删除超过默认保留数量的最旧 JSON 报告；其它文件不属于本模块管理范围。
fn prune_old_reports(report_dir: &Path) -> Result<(), String> {
    let mut reports = fs::read_dir(report_dir)
        .map_err(|error| format!("读取 AI 报告目录失败：{error}"))?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                return None;
            }
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, path))
        })
        .collect::<Vec<_>>();
    reports.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    let remove_count = reports.len().saturating_sub(REPORT_RETENTION_COUNT);
    for (_, path) in reports.into_iter().take(remove_count) {
        fs::remove_file(&path)
            .map_err(|error| format!("清理过期 AI 报告 {} 失败：{error}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造持久化测试使用的最小有效报告。
    fn test_report(session_id: &str, question: &str) -> DiagnosticReport {
        DiagnosticReport {
            session_id: session_id.to_string(),
            question_sha256: question_sha256(question),
            summary: "测试结论".to_string(),
            findings: Vec::new(),
            used_log_profiles: Vec::new(),
            limitations: Vec::new(),
            completed_at: "2026-07-15T00:00:00Z".to_string(),
        }
    }

    /// 验证报告 JSON 不会持久化完整用户问题。
    #[test]
    fn persisted_report_excludes_full_question() {
        let root = tempfile::tempdir().expect("应创建测试目录");
        let question = "包含内部工单 secret-ticket-42 的分析问题";
        let path = persist_report(root.path(), &test_report("privacy-test", question))
            .expect("应保存报告");
        let content = fs::read_to_string(path).expect("应读取报告");
        assert!(!content.contains(question));
        assert!(content.contains(&question_sha256(question)));
    }

    /// 验证报告目录超过 100 份时淘汰最旧文件。
    #[test]
    fn report_retention_keeps_latest_hundred() {
        let root = tempfile::tempdir().expect("应创建测试目录");
        for index in 0..=REPORT_RETENTION_COUNT {
            let session_id = format!("retention-{index:03}");
            persist_report(root.path(), &test_report(&session_id, "retention"))
                .expect("应保存报告");
        }
        let report_count = fs::read_dir(root.path().join("ai/reports"))
            .expect("应读取报告目录")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.path().extension().and_then(|value| value.to_str()) == Some("json")
            })
            .count();
        assert_eq!(report_count, REPORT_RETENTION_COUNT);
    }

    /// 验证“已确认”发现不能在没有来源行证据时通过持久化校验。
    #[test]
    fn confirmed_finding_requires_evidence() {
        let mut report = test_report("confirmed-evidence", "question");
        report.findings.push(DiagnosticFinding {
            title: "确认问题".to_string(),
            severity: "high".to_string(),
            status: DiagnosticFindingStatus::Confirmed,
            analysis: "根据日志判断".to_string(),
            impact: "请求失败".to_string(),
            recommendation: "人工核对配置".to_string(),
            confidence: 0.9,
            evidence: Vec::new(),
            verification_steps: Vec::new(),
        });
        assert!(report.validate().is_err());
    }

    /// 验证证据不足的假设必须给出后续验证方法。
    #[test]
    fn hypothesis_finding_requires_verification_steps() {
        let mut report = test_report("hypothesis-verification", "question");
        report.findings.push(DiagnosticFinding {
            title: "待验证问题".to_string(),
            severity: "medium".to_string(),
            status: DiagnosticFindingStatus::Hypothesis,
            analysis: "当前样本不足".to_string(),
            impact: "可能造成延迟".to_string(),
            recommendation: "补充同时间段日志".to_string(),
            confidence: 0.4,
            evidence: Vec::new(),
            verification_steps: Vec::new(),
        });
        assert!(report.validate().is_err());
    }
}
