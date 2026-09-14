//! 文件职责：Skill 的解析、筛选与注入。
//! 创建日期：2026-09-14
//! 作者：Argus 开发团队
//! 主要功能：内置知识与导入知识统一为 AgentSkill；按产品线默认集和用户禁用列表筛选，
//! 并渲染为进入系统提示词的 <SKILLS> 区块。

use std::path::Path;

use crate::agent::agent_loop::AgentLoopNote;
use crate::config::AiConfig;

/// Skill 正文上限；导入和解析统一按该上限拒绝。
pub(crate) const SKILL_BODY_MAX_BYTES: usize = 32 * 1024;
/// 注入系统提示词的 Skill 区块总预算；超出部分丢弃并记录警告。
pub(crate) const SKILLS_SECTION_MAX_BYTES: usize = 24 * 1024;
/// frontmatter 单行与总行数上限，防止畸形文件拖垮解析。
const SKILL_FRONTMATTER_MAX_LINE_BYTES: usize = 4 * 1024;
const SKILL_FRONTMATTER_MAX_LINES: usize = 64;
/// Skill 名称最大字节数。
const SKILL_NAME_MAX_BYTES: usize = 64;

/// 智能分析默认内置的日志诊断方法论 Skill。
const BUILTIN_LOG_DIAGNOSIS_SKILL: &str = r#"---
name: argus-log-diagnosis
description: Structured methodology for diagnosing incidents from plain log files with only generic workspace tools.
---
# Log diagnosis methodology

Work as an incident analyst. You have three generic tools: `list_loaded_sources`, `read_file`
and a sandboxed `bash`. No specialized analyzers exist; combine them freely.

## Investigation loop
1. **Orient**: call `list_loaded_sources` first. Note the log-type guidance attached to each
   file; it encodes user knowledge about format and meaning.
2. **Scope by time**: logs are usually chronologically ordered. Use line ranges and
   timestamps to bracket the incident window before reading details (`bash` with `head`,
   `tail`, `grep -n` is efficient; `read_file` gives bounded, line-numbered context).
3. **Triage signals**: scan for ERROR/FATAL/exception/OOM/restart/timeout patterns, then
   expand context around the first occurrence of each distinct failure, not every occurrence.
4. **Form hypotheses**: state 2-3 candidate root causes, then seek confirming and refuting
   evidence for each. Prefer hypotheses that explain all observed symptoms with fewest
   assumptions.
5. **Correlate**: cross-check timeline ordering across files (application, GC, access logs)
   to distinguish cause from consequence.
6. **Conclude**: report conclusion, confidence, the evidence (workspace-relative path +
   line numbers) and what remains unverified. Never present an unverified guess as a finding.

## Practical patterns
- Wide grep first (`grep -c`), narrow reads second (`read_file` with `offset_line`).
- Count-based questions (`sort | uniq -c | sort -rn`) beat manual scanning.
- Watch for log rotation and restart boundaries that reset sequence numbers.
- When logs contradict each other, trust the lower-level component and say so explicitly.
"#;

/// 交互助手默认内置的轻量对话辅助 Skill。
const BUILTIN_LOG_CHAT_SKILL: &str = r#"---
name: argus-log-chat
description: Lightweight guidance for conversational log Q&A in the assistant panel.
---
# Conversational log assistance

You chat with a user who has logs loaded in Argus and expects quick, grounded answers.

- Answer the actual question first; add background only when it changes the decision.
- Use `list_loaded_sources` to see what is loaded, and read just enough lines with
  `read_file` / read-only `bash` to support the claim. Cite path and line numbers.
- If the user selected sources with "@", treat them as the focus scope.
- For wide investigations, suggest the dedicated analysis window instead of dumping a
  long report in chat.
- Keep answers short by default; offer to go deeper rather than pre-emptively doing so.
"#;

/// Skill 来源。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SkillOrigin {
    /// 代码内嵌的内置 Skill。
    Builtin,
    /// 用户导入到 config_root/ai/skills 的 Skill。
    Imported,
}

/// 一个 Skill 的内存表示。
#[derive(Clone, Debug)]
pub(crate) struct AgentSkill {
    /// 稳定名称；同时作为禁用列表键和 `<SKILL>` 边界标识。
    pub name: String,
    /// 一句话用途说明；设置页与注入区块使用。
    pub description: String,
    /// Markdown 正文。
    pub body: String,
    /// 来源。
    pub origin: SkillOrigin,
}

/// 解析 SKILL.md 文本；frontmatter 只提取 name 与 description，不引入 YAML 依赖。
pub(crate) fn parse_skill_markdown(content: &str) -> Result<AgentSkill, String> {
    let mut lines = content.lines();
    if lines.next().map(str::trim) != Some("---") {
        return Err("SKILL.md 必须以 --- 开头的 frontmatter".to_string());
    }
    let mut name: Option<String> = None;
    let mut description: Option<String> = None;
    let mut frontmatter_lines = 0_usize;
    let mut reached_end = false;
    for line in lines.by_ref() {
        if line.trim() == "---" {
            reached_end = true;
            break;
        }
        frontmatter_lines += 1;
        if frontmatter_lines > SKILL_FRONTMATTER_MAX_LINES {
            return Err("frontmatter 行数超限".to_string());
        }
        if line.len() > SKILL_FRONTMATTER_MAX_LINE_BYTES {
            return Err("frontmatter 单行超限".to_string());
        }
        if let Some(value) = line.strip_prefix("name:")
            && name.is_none()
        {
            name = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("description:")
            && description.is_none()
        {
            description = Some(value.trim().to_string());
        }
    }
    if !reached_end {
        return Err("frontmatter 缺少结束分隔符 ---".to_string());
    }
    let name = name.unwrap_or_default();
    if name.is_empty() || name.len() > SKILL_NAME_MAX_BYTES {
        return Err("frontmatter 的 name 不能为空且不超过 64 字节".to_string());
    }
    if !name
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
    {
        return Err("name 只能包含字母、数字、-、_ 和 .".to_string());
    }
    let description = description.unwrap_or_default();
    if description.is_empty() {
        return Err("frontmatter 的 description 不能为空".to_string());
    }
    let body = lines.collect::<Vec<_>>().join("\n").trim().to_string();
    if body.len() > SKILL_BODY_MAX_BYTES {
        return Err(format!("正文超过 {} 字节上限", SKILL_BODY_MAX_BYTES));
    }
    Ok(AgentSkill {
        name,
        description,
        body,
        origin: SkillOrigin::Imported,
    })
}

/// 返回两个内置 Skill；随产品线默认集注入且允许用户禁用。
pub(crate) fn builtin_skills() -> Vec<AgentSkill> {
    [BUILTIN_LOG_DIAGNOSIS_SKILL, BUILTIN_LOG_CHAT_SKILL]
        .into_iter()
        .map(|content| {
            let mut skill =
                parse_skill_markdown(content).expect("内置 Skill 文本必须通过自身解析校验");
            skill.origin = SkillOrigin::Builtin;
            skill
        })
        .collect()
}

/// 读取 config_root/ai/skills 下的导入 Skill；单个目录解析失败只降级为警告。
pub(crate) fn load_imported_skills(config_root: &Path) -> (Vec<AgentSkill>, Vec<String>) {
    let skills_dir = config_root.join("ai").join("skills");
    let entries = match std::fs::read_dir(&skills_dir) {
        Ok(entries) => entries,
        Err(_) => return (Vec::new(), Vec::new()),
    };
    let mut skills = Vec::new();
    let mut warnings = Vec::new();
    for entry in entries.flatten() {
        let skill_file = entry.path().join("SKILL.md");
        if !skill_file.is_file() {
            continue;
        }
        match std::fs::read_to_string(&skill_file) {
            Ok(content) => match parse_skill_markdown(&content) {
                Ok(skill) => skills.push(skill),
                Err(error) => warnings.push(format!(
                    "导入 Skill “{}” 解析失败：{error}",
                    entry.file_name().to_string_lossy()
                )),
            },
            Err(error) => warnings.push(format!(
                "导入 Skill “{}” 读取失败：{error}",
                entry.file_name().to_string_lossy()
            )),
        }
    }
    skills.sort_by(|left, right| left.name.cmp(&right.name));
    (skills, warnings)
}

/// 按产品线默认集和用户禁用列表筛选本次会话注入的 Skill。
///
/// 返回值：`(待注入 Skill 列表, 加载警告)`。
pub(crate) fn enabled_skills(
    config: &AiConfig,
    config_root: &Path,
    note: AgentLoopNote,
) -> (Vec<AgentSkill>, Vec<String>) {
    let (imported, warnings) = load_imported_skills(config_root);
    // 导入 Skill 与内置同名时以先注册的内置为准，避免覆盖内置方法论。
    let mut seen_names = std::collections::BTreeSet::new();
    let disabled = config
        .disabled_skills
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let mut selected = Vec::new();
    for skill in builtin_skills().into_iter().chain(imported) {
        let is_default_for_line = match note {
            AgentLoopNote::Analysis => skill.name == "argus-log-diagnosis",
            AgentLoopNote::Assistant => skill.name == "argus-log-chat",
        };
        let is_selected = skill.origin == SkillOrigin::Imported || is_default_for_line;
        if !is_selected || disabled.contains(&skill.name) || !seen_names.insert(skill.name.clone())
        {
            continue;
        }
        selected.push(skill);
    }
    (selected, warnings)
}

/// 把 Skill 列表渲染为系统提示词的 <SKILLS> 区块；超出总预算的尾部 Skill 丢弃。
///
/// 返回值：`(区块文本, 是否发生了截断)`。
pub(crate) fn render_skills_section(skills: &[AgentSkill]) -> (String, bool) {
    if skills.is_empty() {
        return (String::new(), false);
    }
    let mut section = String::from("<SKILLS>\n");
    let mut truncated = false;
    for skill in skills {
        let block = format!(
            "<SKILL name=\"{}\">\n{}\n\n{}\n</SKILL>\n",
            skill.name, skill.description, skill.body
        );
        if section.len() + block.len() + "</SKILLS>\n".len() > SKILLS_SECTION_MAX_BYTES {
            truncated = true;
            break;
        }
        section.push_str(&block);
    }
    section.push_str("</SKILLS>");
    (section, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证合法 frontmatter 与正文被完整解析。
    #[test]
    fn parses_frontmatter_and_body() {
        let skill = parse_skill_markdown(
            "---\nname: my-skill\ndescription: does things\n---\n\n# Guide\n\nUse it well.\n",
        )
        .expect("合法 Skill 应解析成功");
        assert_eq!(skill.name, "my-skill");
        assert_eq!(skill.description, "does things");
        assert!(skill.body.contains("# Guide"));
        assert_eq!(skill.origin, SkillOrigin::Imported);
    }

    /// 验证缺失结束分隔符、非法名称和缺失 description 都被拒绝。
    #[test]
    fn rejects_malformed_frontmatter() {
        assert!(parse_skill_markdown("no frontmatter").is_err());
        assert!(parse_skill_markdown("---\nname: x\n").is_err());
        assert!(parse_skill_markdown("---\nname: bad name\ndescription: d\n---\nbody").is_err());
        assert!(parse_skill_markdown("---\nname: ok\ndescription: d\n---\nbody\n").is_ok());
        assert!(parse_skill_markdown("---\ndescription: d\n---\nbody").is_err());
    }

    /// 验证超限正文被拒绝。
    #[test]
    fn rejects_oversized_body() {
        let body = "x".repeat(SKILL_BODY_MAX_BYTES + 1);
        let content = format!("---\nname: big\ndescription: d\n---\n{body}");
        assert!(parse_skill_markdown(&content).is_err());
    }

    /// 验证内置 Skill 文本可解析且两个产品线默认集筛选正确。
    #[test]
    fn builtin_skills_parse_and_default_sets_filter() {
        let builtins = builtin_skills();
        assert_eq!(builtins.len(), 2);
        assert!(
            builtins
                .iter()
                .all(|skill| skill.origin == SkillOrigin::Builtin)
        );

        let config = AiConfig::default();
        let workspace = tempfile_dir();
        let (analysis, _) = enabled_skills(&config, &workspace, AgentLoopNote::Analysis);
        let (assistant, _) = enabled_skills(&config, &workspace, AgentLoopNote::Assistant);
        assert_eq!(analysis.len(), 1);
        assert_eq!(analysis[0].name, "argus-log-diagnosis");
        assert_eq!(assistant.len(), 1);
        assert_eq!(assistant[0].name, "argus-log-chat");
    }

    /// 验证禁用列表可以移除默认内置 Skill。
    #[test]
    fn disabled_skills_filter_defaults() {
        let config = AiConfig {
            disabled_skills: vec!["argus-log-diagnosis".to_string()],
            ..AiConfig::default()
        };
        let workspace = tempfile_dir();
        let (analysis, _) = enabled_skills(&config, &workspace, AgentLoopNote::Analysis);
        assert!(analysis.is_empty(), "被禁用的内置 Skill 不应注入");
    }

    /// 验证注入区块使用 <SKILL> 边界并在超预算时截断。
    #[test]
    fn renders_section_with_boundaries_and_truncation() {
        let small = AgentSkill {
            name: "small".to_string(),
            description: "d".to_string(),
            body: "keep".to_string(),
            origin: SkillOrigin::Imported,
        };
        let (section, truncated) = render_skills_section(std::slice::from_ref(&small));
        assert!(!truncated);
        assert!(section.starts_with("<SKILLS>"));
        assert!(section.contains("<SKILL name=\"small\">"));
        assert!(section.ends_with("</SKILLS>"));

        let big = AgentSkill {
            name: "big".to_string(),
            description: "d".to_string(),
            body: "y".repeat(SKILLS_SECTION_MAX_BYTES),
            origin: SkillOrigin::Imported,
        };
        let (section, truncated) = render_skills_section(&[big]);
        assert!(truncated);
        assert!(!section.contains("<SKILL name=\"big\">"));
    }

    /// 测试用临时目录；导入加载在空目录上应返回空列表。
    fn tempfile_dir() -> std::path::PathBuf {
        let directory = crate::config::paths::temporary_test_dir("agent-skills");
        directory.path().to_path_buf()
    }
}
