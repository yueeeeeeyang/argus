//! 文件职责：把免审批命令收敛为固定系统程序与经过验证的参数，不执行 shell 语法。

use std::path::{Path, PathBuf};

/// 通过能力检查的程序及参数；执行器必须直接使用 argv，不能再交回 shell 解释。
pub(super) struct ReadonlyCommand {
    pub program: PathBuf,
    pub arguments: Vec<String>,
    pub working_directory: PathBuf,
}

/// 检查只读能力并解析真实文件路径；不支持的命令、选项或 shell 语法转入显式审批。
pub(super) fn prepare_readonly_command(
    command: &str,
    workspace_root: &Path,
) -> Result<ReadonlyCommand, String> {
    let mut words = parse_simple_command(command)?;
    if words.is_empty() {
        return Err("command must not be empty".to_string());
    }
    let name = words.remove(0);
    let program = trusted_program(&name)?;
    let root = workspace_root
        .canonicalize()
        .map_err(|_| "workspace directory unavailable".to_string())?;
    validate_arguments(&name, &mut words, &root)?;
    Ok(ReadonlyCommand {
        program,
        arguments: words,
        working_directory: root,
    })
}

/// 仅使用操作系统安装目录中的已知程序，拒绝 PATH 注入和自定义同名可执行文件。
#[cfg(unix)]
fn trusted_program(name: &str) -> Result<PathBuf, String> {
    if !matches!(
        name,
        "ls" | "cat"
            | "grep"
            | "head"
            | "tail"
            | "wc"
            | "sort"
            | "uniq"
            | "cut"
            | "tr"
            | "echo"
            | "pwd"
            | "stat"
            | "file"
            | "du"
            | "df"
    ) {
        return Err("command requires explicit approval".to_string());
    }
    ["/usr/bin", "/bin"]
        .into_iter()
        .map(|directory| Path::new(directory).join(name))
        .find(|path| path.is_file())
        .ok_or_else(|| "trusted system command is unavailable".to_string())
}

/// Windows 的 Unix 工具来源由用户安装决定，无法证明其只读能力，因此统一要求审批。
#[cfg(not(unix))]
fn trusted_program(_name: &str) -> Result<PathBuf, String> {
    Err("shell commands on this platform require explicit approval".to_string())
}

/// 解析单条命令的引号和转义；任何需要 shell 求值的结构都必须审批。
fn parse_simple_command(command: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut quote = None;
    let mut characters = command.chars();
    while let Some(character) = characters.next() {
        if character == '\0' {
            return Err("command contains a NUL character".to_string());
        }
        match (quote, character) {
            (Some('\''), '\'') | (Some('"'), '"') => quote = None,
            (Some('\''), _) => word.push(character),
            (Some('"'), '$' | '`') => {
                return Err("shell expansion requires approval".to_string());
            }
            (_, '\\') => {
                let escaped = characters.next().ok_or("incomplete shell escape")?;
                if escaped == '\n' || escaped == '\r' || escaped == '\0' {
                    return Err("shell continuation requires approval".to_string());
                }
                word.push(escaped);
                started = true;
            }
            (Some(_), _) => word.push(character),
            (None, '\'' | '"') => {
                quote = Some(character);
                started = true;
            }
            (
                None,
                '$' | '`' | '|' | ';' | '&' | '<' | '>' | '(' | ')' | '{' | '}' | '*' | '?' | '['
                | ']' | '~' | '\n' | '\r',
            ) => {
                return Err("shell operators and expansion require approval".to_string());
            }
            (None, value) if value.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            (None, _) => {
                started = true;
                word.push(character);
            }
        }
    }
    if quote.is_some() {
        return Err("unclosed shell quote".to_string());
    }
    if started {
        words.push(word);
    }
    Ok(words)
}

/// 按程序分别验证参数用途，防止把输出文件、脚本或子进程选项误当成只读参数。
fn validate_arguments(name: &str, arguments: &mut [String], root: &Path) -> Result<(), String> {
    if name == "echo" {
        return Ok(());
    }
    let short_flags = match name {
        "ls" => "laAhR1dFnpStU",
        "cat" => "benstuvAET",
        "grep" => "nivclLwxFEHhosq",
        "head" | "tail" => "qv",
        "wc" => "lwcmL",
        "sort" => "nrufbdhMV",
        "uniq" => "cdui",
        "cut" => "s",
        "tr" => "dscC",
        "pwd" => "LP",
        "stat" => "",
        "file" => "bih",
        "du" => "ashkmc",
        "df" => "hkiPm",
        _ => return Err("command requires explicit approval".to_string()),
    };
    let mut operands = Vec::new();
    let mut index = 0;
    let mut options_ended = false;
    let mut has_grep_pattern = false;
    while index < arguments.len() {
        let argument = arguments[index].as_str();
        if !options_ended && argument == "--" {
            options_ended = true;
        } else if !options_ended && argument.starts_with('-') && argument != "-" {
            let takes_value = matches!(
                (name, argument),
                ("head" | "tail", "-n" | "-c")
                    | ("grep", "-e" | "-f")
                    | ("sort", "-k" | "-t")
                    | ("cut", "-b" | "-c" | "-d" | "-f")
                    | ("du", "-d")
            );
            if takes_value {
                let is_pattern_file = name == "grep" && argument == "-f";
                index += 1;
                let value = arguments
                    .get_mut(index)
                    .ok_or("option is missing its value")?;
                if is_pattern_file {
                    *value = checked_path(value, root)?;
                }
                has_grep_pattern |= name == "grep";
                if matches!(name, "head" | "tail" | "du")
                    && !value
                        .trim_start_matches(['+', '-'])
                        .chars()
                        .all(|digit| digit.is_ascii_digit())
                {
                    return Err("numeric option has an unsupported value".to_string());
                }
            } else if !(matches!(name, "head" | "tail")
                && argument[1..].chars().all(|digit| digit.is_ascii_digit()))
                && !argument[1..].chars().all(|flag| short_flags.contains(flag))
            {
                return Err(format!("option {argument:?} requires explicit approval"));
            }
        } else {
            operands.push(index);
        }
        index += 1;
    }
    if name == "pwd" && !operands.is_empty() {
        return Err("pwd does not accept operands".to_string());
    }
    if name == "tr" {
        return if operands.len() <= 2 {
            Ok(())
        } else {
            Err("unsupported tr operands".to_string())
        };
    }
    if name == "uniq" && operands.len() > 1 {
        return Err("uniq output files require approval".to_string());
    }
    if name == "grep" && !has_grep_pattern {
        if operands.is_empty() {
            return Err("grep is missing a pattern".to_string());
        }
        operands.remove(0);
    }
    for index in operands {
        if arguments[index] != "-" {
            arguments[index] = checked_path(&arguments[index], root)?;
        }
    }
    Ok(())
}

/// canonicalize 同时解析普通相对名称和符号链接；返回真实路径以避免执行时重新跟随原链接。
fn checked_path(argument: &str, root: &Path) -> Result<String, String> {
    let candidate = root.join(argument);
    let canonical = candidate
        .canonicalize()
        .map_err(|_| format!("path cannot be resolved inside the workspace: {argument}"))?;
    if !canonical.starts_with(root) {
        return Err(format!("path leaves the workspace directory: {argument}"));
    }
    Ok(canonical.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 双引号替换和所有具有写入/执行能力的选项必须进入审批，不能只看命令 basename。
    #[test]
    fn rejects_shell_and_program_capability_bypasses() {
        let workspace = tempfile::tempdir().expect("应创建测试目录");
        for command in [
            "echo \"$(touch marker)\"",
            "echo \"`touch marker`\"",
            "awk 'BEGIN {print 1 > \"marker\"}'",
            "sed -n 'w marker' app.log",
            "sort -omarker app.log",
            "uniq app.log marker",
            "find . -exec echo x \\;",
            "PATH=. ls",
            "./ls",
            "/tmp/ls",
            "cat app.log | wc -l",
            "grep --include=\"$(touch marker)\" x app.log",
            "cat app.log > marker",
        ] {
            assert!(
                prepare_readonly_command(command, workspace.path()).is_err(),
                "必须审批：{command}"
            );
        }
    }

    /// 单纯的相对文件名也必须解析符号链接，不能通过省略斜杠逃出工作目录。
    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape_and_keeps_quoted_paths_literal() {
        let workspace = tempfile::tempdir().expect("应创建测试目录");
        let outside = tempfile::NamedTempFile::new().expect("应创建外部文件");
        std::os::unix::fs::symlink(outside.path(), workspace.path().join("linked"))
            .expect("应创建测试链接");
        assert!(prepare_readonly_command("cat linked", workspace.path()).is_err());
        std::fs::write(workspace.path().join("app log"), "fixture").expect("应创建日志");
        let prepared = prepare_readonly_command("grep -n 'a/b' 'app log'", workspace.path())
            .expect("只读命令应成功");
        assert_eq!(prepared.arguments[1], "a/b");
        assert_eq!(
            prepared.arguments[2],
            workspace
                .path()
                .join("app log")
                .canonicalize()
                .expect("文件应存在")
                .to_string_lossy()
        );
    }
}
