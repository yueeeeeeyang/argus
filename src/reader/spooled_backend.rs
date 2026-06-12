//! 文件职责：管理压缩包内超大日志的临时分页文件。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：将不可随机访问的压缩流物化到 `~/.argus/cache/log_pages`，供分页 reader 复用本地文件读取能力。

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};

use crate::config::paths::argus_config_dir;

/// 日志分页缓存目录名。
const ARGUS_CACHE_DIR_NAME: &str = "cache";
/// 压缩日志分页文件子目录名。
const LOG_PAGE_CACHE_DIR_NAME: &str = "log_pages";

/// 临时分页文件清理器；最后一个 reader 句柄释放时自动删除缓存文件。
#[derive(Debug)]
pub struct SpoolCleanup {
    /// 需要清理的临时文件路径。
    path: PathBuf,
}

impl SpoolCleanup {
    /// 创建清理器。
    ///
    /// 参数说明：
    /// - `path`：已写入完成的临时分页文件路径。
    ///
    /// 返回值：可被 reader 句柄共享的清理器。
    pub fn new(path: PathBuf) -> Arc<Self> {
        Arc::new(Self { path })
    }

    /// 返回临时分页文件路径。
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SpoolCleanup {
    /// 后台删除临时分页文件；清理失败不应影响应用退出或 tab 关闭。
    fn drop(&mut self) {
        let path = self.path.clone();
        let _ = thread::Builder::new()
            .name("argus-spool-cleanup".to_string())
            .spawn(move || {
                let _ = fs::remove_file(path);
            });
    }
}

/// 创建新的压缩日志分页临时文件。
///
/// 参数说明：
/// - `label`：日志展示名称，用于生成便于排查的文件名片段。
///
/// 返回值：打开的文件句柄和对应路径；调用方负责写入和 flush。
pub fn create_spool_file(label: &str) -> Result<(File, PathBuf)> {
    let dir = log_page_cache_dir();
    fs::create_dir_all(&dir)
        .with_context(|| format!("无法创建日志分页缓存目录：{}", dir.display()))?;

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let sanitized_label = sanitize_label(label);
    let path = dir.join(format!(
        "argus-{}-{timestamp}-{sanitized_label}.log",
        std::process::id()
    ));
    let file = File::create(&path)
        .with_context(|| format!("无法创建日志分页缓存文件：{}", path.display()))?;

    Ok((file, path))
}

/// 返回压缩日志分页缓存目录。
fn log_page_cache_dir() -> PathBuf {
    argus_config_dir()
        .join(ARGUS_CACHE_DIR_NAME)
        .join(LOG_PAGE_CACHE_DIR_NAME)
}

/// 清理文件名中的路径分隔符和控制字符，避免虚拟路径污染缓存目录。
fn sanitize_label(label: &str) -> String {
    let mut output = label
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if output.is_empty() {
        output.push_str("log");
    }
    output.truncate(80);
    output
}

#[cfg(test)]
mod tests {
    use super::sanitize_label;

    /// 验证虚拟路径会被转换为安全文件名。
    #[test]
    fn sanitizes_virtual_archive_path() {
        assert_eq!(sanitize_label("a/b\\c.log"), "a_b_c.log");
        assert_eq!(sanitize_label("日志"), "__");
    }
}
