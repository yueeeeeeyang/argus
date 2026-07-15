//! 文件职责：提供路径展示与虚拟路径处理工具。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：统一真实路径、压缩包内路径和界面文案之间的转换规则。

use std::path::Path;

/// 返回路径最后一段作为界面标签；无法取得文件名时退回完整路径。
pub(crate) fn display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| path.display().to_string())
}

/// 返回适合状态栏和面包屑显示的路径文本。
pub(crate) fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// 组合压缩包真实路径、嵌套容器链路与内部条目路径，形成稳定的虚拟路径文案。
pub(crate) fn archive_virtual_path(
    archive_path: &Path,
    container_entries: &[String],
    entry_path: &str,
) -> String {
    let mut display_path = archive_path.display().to_string();
    for container_entry in container_entries {
        display_path.push_str("!/");
        display_path.push_str(container_entry);
    }
    display_path.push_str("!/");
    display_path.push_str(entry_path);
    display_path
}

/// 将压缩包条目路径规范化为统一的 `/` 分隔形式。
pub(crate) fn normalize_archive_entry_path(path: &str) -> String {
    path.replace('\\', "/")
        .trim_start_matches('/')
        .trim_end_matches('/')
        .to_string()
}
