//! 文件职责：解析系统外部打开传入的本地路径。
//! 创建日期：2026-06-15
//! 修改日期：2026-06-15
//! 作者：Argus 开发团队
//! 主要功能：把启动参数或系统 `open-url` 事件转换为现有加载器可消费的 `PathBuf` 列表。

use std::ffi::OsString;
use std::path::PathBuf;

use url::Url;

/// 从启动参数提取本地路径。
///
/// 参数说明：
/// - `args`：通常为 `std::env::args_os().skip(1)`，不包含可执行文件路径。
///
/// 返回值：存在于本机路径格式中的候选来源。该函数只解析路径，不做文件系统访问，
/// 由后续加载器负责校验路径是否可读。
pub fn paths_from_startup_args<I>(args: I) -> Vec<PathBuf>
where
    I: IntoIterator<Item = OsString>,
{
    args.into_iter()
        .filter_map(|arg| {
            let text = arg.to_string_lossy();
            path_from_open_string(text.as_ref()).or_else(|| Some(PathBuf::from(arg)))
        })
        .collect()
}

/// 从系统 open-url 事件提取本地路径。
///
/// 参数说明：
/// - `urls`：GPUI 传入的 URL 字符串；Finder/Open With 通常会传入 `file://` URL。
///
/// 返回值：所有可解析为本地文件系统路径的条目。
pub fn paths_from_open_urls(urls: Vec<String>) -> Vec<PathBuf> {
    urls.into_iter()
        .filter_map(|url| path_from_open_string(&url))
        .collect()
}

/// 解析单个外部打开字符串。
///
/// 说明：macOS `open-url` 常用 `file://`，Windows 右键命令通常是普通路径；
/// 因此这里同时兼容 URL 和原始路径两种形式。
pub fn path_from_open_string(value: &str) -> Option<PathBuf> {
    if value.trim().is_empty() {
        return None;
    }

    if let Ok(url) = Url::parse(value)
        && url.scheme() == "file"
    {
        return url.to_file_path().ok();
    }

    Some(PathBuf::from(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 file URL 能转换为本地路径，避免 macOS Open With 入口无法进入来源加载流程。
    #[test]
    fn parses_file_url_as_path() {
        let path = path_from_open_string("file:///tmp/argus.log").expect("file url should parse");
        assert_eq!(path, PathBuf::from("/tmp/argus.log"));
    }

    /// 验证普通路径会原样保留，兼容 Windows 注册表右键命令传入的 `%1`。
    #[test]
    fn keeps_plain_path() {
        let path = path_from_open_string("/tmp/plain.log").expect("plain path should parse");
        assert_eq!(path, PathBuf::from("/tmp/plain.log"));
    }
}
