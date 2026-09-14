//! 文件职责：Skill 的解析、筛选与注入。
//! 创建日期：2026-09-14
//! 修改日期：2026-09-14
//! 作者：Argus 开发团队
//! 主要功能：导入知识解析为 AgentSkill；按用户禁用列表筛选，并渲染为进入系统提示词的
//! <SKILLS> 区块。不提供任何内置 Skill。

use std::path::{Path, PathBuf};

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

/// 一个 Skill 的内存表示。
#[derive(Clone, Debug)]
pub(crate) struct AgentSkill {
    /// 稳定名称；同时作为禁用列表键和 `<SKILL>` 边界标识。
    pub name: String,
    /// 一句话用途说明；设置页与注入区块使用。
    pub description: String,
    /// Markdown 正文。
    pub body: String,
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
    })
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

/// 按用户禁用列表筛选本次会话注入的导入 Skill；同名目录以名称排序的第一份为准。
///
/// 返回值：`(待注入 Skill 列表, 加载警告)`。
pub(crate) fn enabled_skills(
    config: &AiConfig,
    config_root: &Path,
) -> (Vec<AgentSkill>, Vec<String>) {
    let (imported, warnings) = load_imported_skills(config_root);
    let mut seen_names = std::collections::BTreeSet::new();
    let disabled = config
        .disabled_skills
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let mut selected = Vec::new();
    for skill in imported {
        if disabled.contains(&skill.name) || !seen_names.insert(skill.name.clone()) {
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

/// 导入 Skill 的单文件数量与总字节上限。
pub(crate) const SKILL_IMPORT_MAX_FILES: usize = 64;
pub(crate) const SKILL_IMPORT_MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024;

/// 待落盘的一个导入文件。
struct ImportFile {
    /// 相对 Skill 根的路径（已清洗）。
    relative_path: PathBuf,
    /// 文件内容。
    bytes: Vec<u8>,
}

/// 从目录或 .zip 导入一个 Skill，复制到 config_root/ai/skills/<name>/。
///
/// 参数说明：
/// - `source`：包含 SKILL.md 的目录，或顶层目录内含 SKILL.md 的 .zip 文件；
/// - `config_root`：settings.toml 所在目录，导入产物写入其 ai/skills 子目录。
///
/// 返回值：导入成功后的 Skill 名称。任何校验失败都不留残留目录。
pub(crate) fn import_skill_from_path(source: &Path, config_root: &Path) -> Result<String, String> {
    let files = if source.is_file() {
        collect_zip_skill_files(source)?
    } else if source.is_dir() {
        collect_directory_skill_files(source)?
    } else {
        return Err(format!("导入路径不存在：{}", source.display()));
    };
    let skill_markdown = files
        .iter()
        .find(|file| file.relative_path == Path::new("SKILL.md"))
        .ok_or_else(|| "导入内容缺少 SKILL.md".to_string())?;
    let skill = parse_skill_markdown(&String::from_utf8_lossy(&skill_markdown.bytes))?;
    // 与既有导入同名时拒绝导入，避免静默覆盖或注入歧义。
    let existing_names = load_imported_skills(config_root)
        .0
        .into_iter()
        .map(|existing| existing.name)
        .collect::<std::collections::BTreeSet<_>>();
    if existing_names.contains(&skill.name) {
        return Err(format!("Skill 名称“{}”已存在，导入被拒绝", skill.name));
    }
    let target_dir = config_root.join("ai").join("skills").join(&skill.name);
    if target_dir.exists() {
        return Err(format!("目标目录已存在：{}", target_dir.display()));
    }
    std::fs::create_dir_all(&target_dir)
        .map_err(|error| format!("创建 Skill 目录失败：{error}"))?;
    for file in &files {
        let destination = target_dir.join(&file.relative_path);
        if let Some(parent) = destination.parent()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            let _ = std::fs::remove_dir_all(&target_dir);
            return Err(format!("创建 Skill 子目录失败：{error}"));
        }
        if let Err(error) = std::fs::write(&destination, &file.bytes) {
            let _ = std::fs::remove_dir_all(&target_dir);
            return Err(format!("写入 Skill 文件失败：{error}"));
        }
    }
    Ok(skill.name)
}

/// 删除一个导入 Skill 的目录；返回是否存在该目录。
pub(crate) fn remove_imported_skill(name: &str, config_root: &Path) -> Result<bool, String> {
    let target_dir = config_root.join("ai").join("skills").join(name);
    if !target_dir.exists() {
        return Ok(false);
    }
    std::fs::remove_dir_all(&target_dir)
        .map_err(|error| format!("删除 Skill 目录失败：{error}"))?;
    Ok(true)
}

/// 递归收集目录内的 Skill 文件；拒绝符号链接并应用数量与字节预算。
fn collect_directory_skill_files(source: &Path) -> Result<Vec<ImportFile>, String> {
    let mut files = Vec::new();
    let mut total_bytes = 0_u64;
    let mut stack = vec![(source.to_path_buf(), PathBuf::new())];
    while let Some((absolute, relative)) = stack.pop() {
        let entries =
            std::fs::read_dir(&absolute).map_err(|error| format!("读取目录失败：{error}"))?;
        for entry in entries.flatten() {
            let metadata = std::fs::symlink_metadata(entry.path())
                .map_err(|error| format!("读取文件元数据失败：{error}"))?;
            if metadata.file_type().is_symlink() {
                return Err("导入目录包含符号链接，已拒绝".to_string());
            }
            let entry_relative = relative.join(entry.file_name());
            if metadata.is_dir() {
                stack.push((entry.path(), entry_relative));
                continue;
            }
            total_bytes = total_bytes
                .checked_add(metadata.len())
                .ok_or("导入内容字节数溢出")?;
            if total_bytes > SKILL_IMPORT_MAX_TOTAL_BYTES {
                return Err(format!(
                    "导入内容超过 {} 字节上限",
                    SKILL_IMPORT_MAX_TOTAL_BYTES
                ));
            }
            files.push(ImportFile {
                relative_path: entry_relative,
                bytes: std::fs::read(entry.path())
                    .map_err(|error| format!("读取文件失败：{error}"))?,
            });
            if files.len() > SKILL_IMPORT_MAX_FILES {
                return Err(format!("导入文件数超过 {} 上限", SKILL_IMPORT_MAX_FILES));
            }
        }
    }
    relocate_skill_root(files)
}

/// 读取 zip 并收集 Skill 文件；条目名先做 zip slip 清洗再定位唯一顶层目录。
fn collect_zip_skill_files(source: &Path) -> Result<Vec<ImportFile>, String> {
    let file = std::fs::File::open(source).map_err(|error| format!("打开 ZIP 失败：{error}"))?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|error| format!("读取 ZIP 结构失败：{error}"))?;
    let mut files = Vec::new();
    let mut total_bytes = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("读取 ZIP 条目失败：{error}"))?;
        if entry.is_dir() {
            continue;
        }
        if entry.name().starts_with('/') || entry.name().contains("..") {
            return Err(format!("ZIP 条目路径不安全：{}", entry.name()));
        }
        total_bytes = total_bytes
            .checked_add(entry.size())
            .ok_or("导入内容字节数溢出")?;
        if total_bytes > SKILL_IMPORT_MAX_TOTAL_BYTES {
            return Err(format!(
                "导入内容超过 {} 字节上限",
                SKILL_IMPORT_MAX_TOTAL_BYTES
            ));
        }
        let relative = PathBuf::from(entry.name().replace('\\', "/"));
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        std::io::Read::read_to_end(&mut entry, &mut bytes)
            .map_err(|error| format!("解压条目失败：{error}"))?;
        files.push(ImportFile {
            relative_path: relative,
            bytes,
        });
        if files.len() > SKILL_IMPORT_MAX_FILES {
            return Err(format!("导入文件数超过 {} 上限", SKILL_IMPORT_MAX_FILES));
        }
    }
    relocate_skill_root(files)
}

/// 把文件列表的公共顶层目录剥掉，使 SKILL.md 位于导入根。
fn relocate_skill_root(mut files: Vec<ImportFile>) -> Result<Vec<ImportFile>, String> {
    let top_components = files
        .iter()
        .filter_map(|file| file.relative_path.components().next())
        .filter(|component| {
            !matches!(
                component,
                std::path::Component::CurDir | std::path::Component::RootDir
            )
        })
        .map(|component| component.as_os_str().to_os_string())
        .collect::<std::collections::BTreeSet<_>>();
    let has_root_skill = files
        .iter()
        .any(|file| file.relative_path == Path::new("SKILL.md"));
    if top_components.len() > 1 && !has_root_skill {
        return Err("ZIP 内存在多个顶层目录且没有根级 SKILL.md".to_string());
    }
    if top_components.len() == 1 && !has_root_skill {
        let top = top_components.into_iter().next().expect("已确认唯一顶层");
        for file in &mut files {
            file.relative_path = file
                .relative_path
                .strip_prefix(&top)
                .map_err(|_| "剥离顶层目录失败".to_string())?
                .to_path_buf();
        }
    }
    if !files
        .iter()
        .any(|file| file.relative_path == Path::new("SKILL.md"))
    {
        return Err("导入内容缺少 SKILL.md".to_string());
    }
    files.retain(|file| !file.relative_path.as_os_str().is_empty());
    Ok(files)
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

    /// 验证无导入时筛选结果为空；导入后按禁用列表过滤。
    #[test]
    fn enabled_skills_returns_imported_minus_disabled() {
        let home = tempfile_dir("enabled-filter");
        let home = home.path();
        let config = AiConfig::default();
        let (empty, _) = enabled_skills(&config, home);
        assert!(empty.is_empty(), "没有任何导入 Skill 时不应注入");

        let source = home.join("src-skill");
        std::fs::create_dir_all(&source).expect("应创建源目录");
        std::fs::write(
            source.join("SKILL.md"),
            "---\nname: filterable\ndescription: d\n---\nbody\n",
        )
        .expect("应写入 SKILL.md");
        import_skill_from_path(&source, home).expect("导入应成功");

        let (selected, _) = enabled_skills(&config, home);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].name, "filterable");

        let config = AiConfig {
            disabled_skills: vec!["filterable".to_string()],
            ..AiConfig::default()
        };
        let (filtered, _) = enabled_skills(&config, home);
        assert!(filtered.is_empty(), "被禁用的 Skill 不应注入");
    }

    /// 验证注入区块使用 <SKILL> 边界并在超预算时截断。
    #[test]
    fn renders_section_with_boundaries_and_truncation() {
        let small = AgentSkill {
            name: "small".to_string(),
            description: "d".to_string(),
            body: "keep".to_string(),
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
        };
        let (section, truncated) = render_skills_section(&[big]);
        assert!(truncated);
        assert!(!section.contains("<SKILL name=\"big\">"));
    }

    /// 验证从目录导入 Skill 后可被加载，重复导入同名被拒绝。
    #[test]
    fn imports_skill_from_directory_and_rejects_duplicates() {
        let home = tempfile_dir("import-dir");
        let home = home.path();
        let source = home.join("my-skill-src");
        std::fs::create_dir_all(source.join("notes")).expect("应创建源目录");
        std::fs::write(
            source.join("SKILL.md"),
            "---\nname: my-imported-skill\ndescription: imported for test\n---\nUse it.\n",
        )
        .expect("应写入 SKILL.md");
        std::fs::write(source.join("notes/extra.md"), "extra").expect("应写入附加文件");

        let name = import_skill_from_path(&source, home).expect("目录导入应成功");
        assert_eq!(name, "my-imported-skill");
        let (loaded, warnings) = load_imported_skills(home);
        assert!(warnings.is_empty());
        assert!(loaded.iter().any(|skill| skill.name == "my-imported-skill"));
        assert!(
            home.join("ai/skills/my-imported-skill/notes/extra.md")
                .is_file()
        );

        let duplicate = import_skill_from_path(&source, home);
        assert!(duplicate.is_err(), "同名重复导入必须被拒绝");
        assert!(remove_imported_skill("my-imported-skill", home).expect("删除应成功"));
        assert!(load_imported_skills(home).0.is_empty());
    }

    /// 验证 zip 导入定位唯一顶层目录，zip slip 条目被拒绝。
    #[test]
    fn imports_skill_from_zip_and_rejects_zip_slip() {
        let home = tempfile_dir("import-zip");
        let home = home.path();
        let zip_path = home.join("skill.zip");
        {
            use std::io::Write as _;
            use zip::ZipWriter;
            use zip::write::SimpleFileOptions;
            let file = std::fs::File::create(&zip_path).expect("应创建 ZIP");
            let mut writer = ZipWriter::new(file);
            writer
                .start_file("top/SKILL.md", SimpleFileOptions::default())
                .expect("应创建条目");
            writer
                .write_all(b"---\nname: zipped-skill\ndescription: from zip\n---\nZipped body.\n")
                .expect("应写入条目");
            writer
                .start_file("top/guide.md", SimpleFileOptions::default())
                .expect("应创建条目");
            writer.write_all(b"guide").expect("应写入条目");
            writer.finish().expect("应完成 ZIP");
        }

        let name = import_skill_from_path(&zip_path, home).expect("ZIP 导入应成功");
        assert_eq!(name, "zipped-skill");
        assert!(home.join("ai/skills/zipped-skill/guide.md").is_file());

        // zip slip：条目路径包含 .. 时必须拒绝且不留残留。
        let evil_path = home.join("evil.zip");
        {
            use std::io::Write as _;
            use zip::ZipWriter;
            use zip::write::SimpleFileOptions;
            let file = std::fs::File::create(&evil_path).expect("应创建 ZIP");
            let mut writer = ZipWriter::new(file);
            writer
                .start_file("../evil.txt", SimpleFileOptions::default())
                .expect("应创建条目");
            writer.write_all(b"x").expect("应写入条目");
            writer.finish().expect("应完成 ZIP");
        }
        assert!(import_skill_from_path(&evil_path, home).is_err());
        assert!(!home.join("evil.txt").exists());
    }

    /// 验证导入来源缺少 SKILL.md 时被拒绝。
    #[test]
    fn import_requires_skill_markdown() {
        let home = tempfile_dir("import-missing");
        let home = home.path();
        let source = home.join("no-skill");
        std::fs::create_dir_all(&source).expect("应创建目录");
        std::fs::write(source.join("readme.md"), "no skill here").expect("应写入文件");
        assert!(import_skill_from_path(&source, home).is_err());
    }

    /// 按测试名隔离的临时目录；守卫保持在测试栈上，目录随测试结束清理。
    fn tempfile_dir(name: &str) -> tempfile::TempDir {
        crate::config::paths::temporary_test_dir(&format!("agent-skills-{name}"))
    }
}
