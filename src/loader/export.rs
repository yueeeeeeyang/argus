//! 文件职责：把已物化的来源文件导出（另存为）到用户指定的目标目录。
//! 创建日期：2026-10-08
//! 修改日期：2026-10-08
//! 作者：Argus 开发团队
//! 主要功能：以流式复制把单个来源文件写到目标目录，并在同名冲突时生成不覆盖既有文件的新名称。

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};

/// 导出复制缓冲区大小；与工作目录物化保持同一量级，避免大文件导出时产生大量系统调用。
const EXPORT_BUFFER_BYTES: usize = 64 * 1024;
/// 同名冲突时尝试的最大序号；超过则判定目标目录异常并明确失败。
const MAX_NAME_CONFLICT_SUFFIX: usize = 9999;

/// 一次单文件导出的结果。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SourceExportOutcome {
    /// 实际写入的目标路径；发生同名冲突时文件名带有序号，与请求的名称不同。
    pub target_path: PathBuf,
    /// 复制的字节数。
    pub copied_bytes: u64,
    /// 是否因目标目录已存在同名文件而改用了带序号的新文件名。
    pub used_fallback_name: bool,
}

/// 把单个来源文件另存到目标目录。
///
/// 参数说明：
/// - `source_path`：已物化的来源文件路径，必须是普通文件。
/// - `target_dir`：用户选择的目标目录，必须已存在且是目录。
///
/// 返回值：写入成功时返回实际目标路径与复制字节数；来源不是普通文件、目标不是目录或
/// 复制过程失败时返回带路径上下文的错误。
///
/// 说明：目标文件使用 `create_new` 创建，因此**绝不覆盖**目标目录中的既有文件；
/// 同名时自动追加 ` (n)` 序号（与来源物化阶段的命名规则一致）。
pub(crate) fn export_source_file(
    source_path: &Path,
    target_dir: &Path,
) -> Result<SourceExportOutcome> {
    if !source_path.is_file() {
        bail!("来源不是可另存的普通文件：{}", source_path.display());
    }
    if !target_dir.is_dir() {
        bail!("目标不是有效目录：{}", target_dir.display());
    }
    let Some(file_name) = source_path.file_name().and_then(|name| name.to_str()) else {
        bail!("来源文件名无效，无法另存：{}", source_path.display());
    };

    let mut source = File::open(source_path)
        .with_context(|| format!("无法打开来源文件：{}", source_path.display()))?;

    let (target, mut handle) = create_unique_target(target_dir, file_name)?;
    let mut copied_bytes = 0_u64;
    let mut buffer = vec![0_u8; EXPORT_BUFFER_BYTES];

    loop {
        let read = source
            .read(&mut buffer)
            .with_context(|| format!("读取来源文件失败：{}", source_path.display()))?;
        if read == 0 {
            break;
        }
        // 单个目标文件在写入过程中失败时删除半成品，避免用户误以为导出成功。
        if let Err(error) = handle.write_all(&buffer[..read]) {
            drop(handle);
            let _ = fs::remove_file(&target);
            return Err(error).with_context(|| format!("写入目标文件失败：{}", target.display()));
        }
        copied_bytes = copied_bytes.saturating_add(read as u64);
    }

    // 先 flush 再关闭，确保缓冲区内容落盘后才向用户报告成功。
    if let Err(error) = handle.flush() {
        drop(handle);
        let _ = fs::remove_file(&target);
        return Err(error).with_context(|| format!("写入目标文件失败：{}", target.display()));
    }
    drop(handle);

    // 目标大小与读取字节数不一致说明写入不完整，按失败处理而不是报告成功。
    if let Ok(metadata) = fs::metadata(&target)
        && metadata.len() != copied_bytes
    {
        let _ = fs::remove_file(&target);
        bail!(
            "导出校验失败：目标文件 {} 写入 {} 字节，预期 {} 字节",
            target.display(),
            metadata.len(),
            copied_bytes
        );
    }

    Ok(SourceExportOutcome {
        used_fallback_name: target.file_name() != Some(file_name.as_ref()),
        target_path: target,
        copied_bytes,
    })
}

/// 在目标目录中以 `create_new` 创建不冲突的目标文件。
///
/// 返回值：实际创建的文件路径及其可写句柄；同名冲突时按 `名称 (2).扩展名` 递增重试。
fn create_unique_target(target_dir: &Path, file_name: &str) -> Result<(PathBuf, File)> {
    let (stem, extension) = split_file_name_extension(file_name);
    let mut candidate = file_name.to_string();

    for suffix in 1..=MAX_NAME_CONFLICT_SUFFIX {
        if suffix > 1 {
            candidate = format!("{stem} ({suffix}){extension}");
        }
        let path = target_dir.join(&candidate);
        match open_new_file(&path) {
            Ok(handle) => return Ok((path, handle)),
            // 目标已存在：换下一个序号，绝不覆盖既有文件。
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("无法在目标目录创建文件：{}", path.display()));
            }
        }
    }

    bail!(
        "目标目录 {} 中同名文件过多，无法生成可用的新文件名：{file_name}",
        target_dir.display()
    )
}

/// 以独占方式创建目标文件。
fn open_new_file(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

/// 拆分文件名的主干与扩展名；无扩展名时扩展名为空串。
///
/// 说明：与来源物化阶段的拆分规则保持一致，`archive.tar.gz` 的扩展名按 `.gz` 处理。
fn split_file_name_extension(file_name: &str) -> (String, String) {
    match file_name.rfind('.') {
        // 前导点开头的隐藏文件（如 `.env`）整体视为主干，不产生空主干。
        Some(index) if index > 0 => (
            file_name[..index].to_string(),
            file_name[index..].to_string(),
        ),
        _ => (file_name.to_string(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::paths::isolated_test_dir;

    /// 创建独立的来源文件与目标目录。
    fn prepare_case(scope: &str) -> (PathBuf, PathBuf) {
        let root = isolated_test_dir(scope);
        let source_dir = root.join("source");
        let target_dir = root.join("target");
        fs::create_dir_all(&source_dir).expect("应能创建来源目录");
        fs::create_dir_all(&target_dir).expect("应能创建目标目录");
        (source_dir, target_dir)
    }

    /// 验证导出会复制完整内容并保留原文件名。
    #[test]
    fn export_copies_content_and_keeps_file_name() {
        let (source_dir, target_dir) = prepare_case("export-basic");
        let source = source_dir.join("app.log");
        let content = b"first line\nsecond line\n";
        fs::write(&source, content).expect("应能写入来源文件");

        let outcome = export_source_file(&source, &target_dir).expect("导出应成功");

        assert_eq!(outcome.target_path, target_dir.join("app.log"));
        assert_eq!(outcome.copied_bytes, content.len() as u64);
        assert!(!outcome.used_fallback_name);
        assert_eq!(fs::read(&outcome.target_path).expect("应能读回"), content);
        // 来源文件必须保持原样，导出是复制而不是移动。
        assert_eq!(fs::read(&source).expect("来源应仍在"), content);
    }

    /// 验证同名目标不会被覆盖，而是追加序号生成新文件名。
    #[test]
    fn export_never_overwrites_existing_target() {
        let (source_dir, target_dir) = prepare_case("export-collision");
        let source = source_dir.join("app.log");
        fs::write(&source, b"new").expect("应能写入来源文件");
        let occupied = target_dir.join("app.log");
        fs::write(&occupied, b"keep-me").expect("应能写入既有目标文件");

        let outcome = export_source_file(&source, &target_dir).expect("导出应成功");

        assert_eq!(outcome.target_path, target_dir.join("app (2).log"));
        assert!(outcome.used_fallback_name);
        assert_eq!(
            fs::read(&occupied).expect("既有文件应保留"),
            b"keep-me",
            "既有目标文件绝不能被覆盖"
        );
        assert_eq!(fs::read(&outcome.target_path).expect("应能读回"), b"new");
    }

    /// 验证连续冲突时序号会继续递增。
    #[test]
    fn export_increments_suffix_for_repeated_collisions() {
        let (source_dir, target_dir) = prepare_case("export-collision-chain");
        let source = source_dir.join("app.log");
        fs::write(&source, b"payload").expect("应能写入来源文件");
        fs::write(target_dir.join("app.log"), b"a").expect("应能写入");
        fs::write(target_dir.join("app (2).log"), b"b").expect("应能写入");

        let outcome = export_source_file(&source, &target_dir).expect("导出应成功");
        assert_eq!(outcome.target_path, target_dir.join("app (3).log"));
    }

    /// 验证无扩展名与隐藏文件的命名拆分不会产生空主干。
    #[test]
    fn export_handles_names_without_extension() {
        let (source_dir, target_dir) = prepare_case("export-no-extension");
        let source = source_dir.join("thread-dump");
        fs::write(&source, b"dump").expect("应能写入来源文件");
        fs::write(target_dir.join("thread-dump"), b"x").expect("应能写入");

        let outcome = export_source_file(&source, &target_dir).expect("导出应成功");
        assert_eq!(outcome.target_path, target_dir.join("thread-dump (2)"));
    }

    /// 验证来源不是普通文件、目标是文件或目标不存在时都会明确失败。
    #[test]
    fn export_rejects_invalid_source_and_target() {
        let (source_dir, target_dir) = prepare_case("export-invalid");
        let source = source_dir.join("app.log");
        fs::write(&source, b"content").expect("应能写入来源文件");
        let target_file = target_dir.join("occupied.txt");
        fs::write(&target_file, b"x").expect("应能写入普通文件");

        assert!(
            export_source_file(&source_dir, &target_dir).is_err(),
            "目录不能作为来源"
        );
        assert!(
            export_source_file(&source, &target_file).is_err(),
            "普通文件不能作为目标目录"
        );
        assert!(
            export_source_file(&source, &target_dir.join("missing")).is_err(),
            "不存在的目标目录必须明确失败"
        );
        assert!(
            export_source_file(&source_dir.join("missing.log"), &target_dir).is_err(),
            "不存在的来源必须明确失败"
        );
    }

    /// 验证隐藏文件（前导点）的命名拆分规则。
    #[test]
    fn file_name_split_keeps_leading_dot_as_stem() {
        assert_eq!(
            split_file_name_extension(".env"),
            (".env".to_string(), String::new())
        );
        assert_eq!(
            split_file_name_extension("archive.tar.gz"),
            ("archive.tar".to_string(), ".gz".to_string())
        );
    }
}
