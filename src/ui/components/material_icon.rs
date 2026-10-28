//! 文件职责：定义来源树使用的 Material Icon Theme 彩色图标子集与名称解析规则。
//! 创建日期：2026-10-28
//! 修改日期：2026-10-28
//! 作者：Argus 开发团队
//! 主要功能：按文件名/扩展名选择 Material Icon Theme 图标，并以彩色 PNG 渲染。

use gpui::{IntoElement, img, prelude::*, px};

/// 一个内嵌的 Material Icon Theme 图标。
#[derive(Debug)]
pub(crate) struct MaterialIcon {
    /// 上游图标 ID，同时作为文件名。
    id: &'static str,
    /// GPUI 资产路径。
    path: &'static str,
    /// 编译期内嵌的 64×64 透明 PNG。
    png: &'static [u8],
}

/// 声明一个内嵌图标：路径与 PNG 字节均由 ID 推导，避免两处手写路径漂移。
macro_rules! material_icon {
    ($id:literal) => {
        MaterialIcon {
            id: $id,
            path: concat!("icons/material/", $id, ".png"),
            png: include_bytes!(concat!("../../../assets/icons/material/", $id, ".png")),
        }
    };
}

/// 来源树使用的全部图标。
///
/// 说明：图标来自 https://github.com/material-extensions/vscode-material-icon-theme
/// 5.38.1 的 dark theme（MIT），只保留日志分析目录树实际会遇到的类型。
/// 目录统一使用第一版的棕色 `folder-base`；文件按名称/扩展名细分。
/// 版权与许可全文见 `assets/icons/material/LICENSE`。
pub(crate) static MATERIAL_ICONS: &[MaterialIcon] = &[
    material_icon!("file"),
    material_icon!("folder-base"),
    material_icon!("folder-base-open"),
    material_icon!("log"),
    material_icon!("zip"),
    material_icon!("settings"),
    material_icon!("xml"),
    material_icon!("json"),
    material_icon!("yaml"),
    material_icon!("toml"),
    material_icon!("document"),
    material_icon!("markdown"),
    material_icon!("console"),
    material_icon!("jar"),
    material_icon!("table"),
    material_icon!("database"),
    material_icon!("lock"),
    material_icon!("changelog"),
    material_icon!("docker"),
    material_icon!("makefile"),
    material_icon!("git"),
    material_icon!("license"),
    material_icon!("readme"),
    material_icon!("maven"),
    material_icon!("go-mod"),
];

impl MaterialIcon {
    /// 返回所有已内嵌的图标。
    pub(crate) fn all() -> &'static [Self] {
        MATERIAL_ICONS
    }

    /// 返回上游图标 ID；仅供测试断言语义映射。
    #[cfg(test)]
    pub(crate) fn id(&self) -> &'static str {
        self.id
    }

    /// 返回 GPUI 资产路径。
    pub(crate) fn path(&self) -> &'static str {
        self.path
    }

    /// 返回资产目录列表使用的文件名。
    pub(crate) fn file_name(&self) -> &'static str {
        self.path.trim_start_matches("icons/material/")
    }

    /// 返回编译期内嵌的 PNG 字节。
    pub(crate) fn bytes(&self) -> &'static [u8] {
        self.png
    }

    /// 按图标 ID 查找。
    pub(crate) fn by_id(id: &str) -> Option<&'static Self> {
        MATERIAL_ICONS.iter().find(|icon| icon.id == id)
    }

    /// 根据 GPUI 请求路径反查图标。
    pub(crate) fn from_path(path: &str) -> Option<&'static Self> {
        MATERIAL_ICONS.iter().find(|icon| icon.path == path)
    }
}

/// 精确文件名映射（小写）。
const FILE_NAME_ICONS: &[(&str, &str)] = &[
    ("dockerfile", "docker"),
    ("makefile", "makefile"),
    (".gitignore", "git"),
    ("license", "license"),
    ("license.md", "license"),
    ("readme", "readme"),
    ("readme.md", "readme"),
    ("pom.xml", "maven"),
    ("go.mod", "go-mod"),
];

/// 扩展名映射（小写，不含前导点）。
const FILE_EXTENSION_ICONS: &[(&str, &str)] = &[
    ("log", "log"),
    ("zip", "zip"),
    ("gz", "zip"),
    ("tar", "zip"),
    ("tgz", "zip"),
    ("rar", "zip"),
    ("7z", "zip"),
    ("properties", "settings"),
    ("ini", "settings"),
    ("conf", "settings"),
    ("cfg", "settings"),
    ("xml", "xml"),
    ("json", "json"),
    ("yaml", "yaml"),
    ("yml", "yaml"),
    ("toml", "toml"),
    ("txt", "document"),
    ("md", "markdown"),
    ("markdown", "markdown"),
    ("sh", "console"),
    ("bash", "console"),
    ("zsh", "console"),
    ("jar", "jar"),
    ("csv", "table"),
    ("sql", "database"),
    ("db", "database"),
    ("lock", "lock"),
];

/// 按 ID 取图标；映射表写错时回退到默认文件图标，测试会断言所有 ID 均存在。
fn icon(id: &str) -> &'static MaterialIcon {
    MaterialIcon::by_id(id)
        .or_else(|| MaterialIcon::by_id("file"))
        .expect("必须内置默认 file 图标")
}

/// 返回压缩包图标。
pub(crate) fn zip_icon() -> &'static MaterialIcon {
    icon("zip")
}

/// 返回目录图标：统一使用第一版的棕色文件夹，仅区分收起/展开。
pub(crate) fn folder_icon(expanded: bool) -> &'static MaterialIcon {
    icon(if expanded {
        "folder-base-open"
    } else {
        "folder-base"
    })
}

/// 返回 SSH 链接图标。
pub(crate) fn ssh_icon() -> &'static MaterialIcon {
    icon("console")
}

/// 返回 SMB 链接图标。
pub(crate) fn smb_icon() -> &'static MaterialIcon {
    icon("database")
}

/// 返回 Git 链接图标。
pub(crate) fn git_icon() -> &'static MaterialIcon {
    icon("git")
}

/// 返回 SVN 链接图标；Material Icon Theme 没有 SVN 专用图标，使用版本历史图。
pub(crate) fn svn_icon() -> &'static MaterialIcon {
    icon("changelog")
}

/// 解析文件图标：精确文件名 → 压缩包/日志特判 → 最长扩展名 → 默认 file。
pub(crate) fn file_icon_for_name(name: &str) -> &'static MaterialIcon {
    let lower = name.to_ascii_lowercase();
    if let Some((_, id)) = FILE_NAME_ICONS.iter().find(|(key, _)| *key == lower) {
        return icon(id);
    }
    if has_archive_extension(&lower) {
        return icon("zip");
    }
    if looks_like_log(&lower) {
        return icon("log");
    }

    // 从第一个非首字符的点开始，优先匹配更长的复合扩展名（tar.gz 先试 tar.gz 再试 gz）。
    let mut search_from = 1;
    while let Some(offset) = lower.get(search_from..).and_then(|rest| rest.find('.')) {
        let dot = search_from + offset;
        let extension = &lower[dot + 1..];
        if let Some((_, id)) = FILE_EXTENSION_ICONS
            .iter()
            .find(|(key, _)| *key == extension)
        {
            return icon(id);
        }
        search_from = dot + 1;
    }

    icon("file")
}

/// 判断文件名是否带压缩包扩展名。
fn has_archive_extension(lower: &str) -> bool {
    [
        ".zip", ".gz", ".tar", ".tgz", ".tbz2", ".bz2", ".txz", ".xz", ".7z", ".rar",
    ]
    .iter()
    .any(|suffix| lower.ends_with(suffix))
}

/// 判断文件名是否按日志命名，兼容轮转日志（app.log.1、app.log-2026-10-27）。
fn looks_like_log(lower: &str) -> bool {
    lower.ends_with(".log") || lower.contains(".log.") || lower.contains(".log-")
}

/// 渲染 Material Icon Theme 彩色图标。
///
/// 使用 `img()` 按位图渲染：GPUI 的 SVG 通路是单色蒙版，会丢失图标内部配色。
pub(crate) fn render_material_icon(icon: &'static MaterialIcon, size: f32) -> impl IntoElement {
    img(icon.path()).size(px(size))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{
        FILE_EXTENSION_ICONS, FILE_NAME_ICONS, MaterialIcon, file_icon_for_name, folder_icon,
        git_icon, smb_icon, ssh_icon, svn_icon,
    };

    /// 验证资产路径唯一、可按路径反查且 PNG 内容非空。
    #[test]
    fn material_icon_paths_are_unique_and_loadable() {
        let mut paths = HashSet::new();
        let mut ids = HashSet::new();
        for icon in MaterialIcon::all() {
            assert!(paths.insert(icon.path()), "图标路径重复：{}", icon.path());
            assert!(ids.insert(icon.id()), "图标 ID 重复：{}", icon.id());
            assert_eq!(
                MaterialIcon::from_path(icon.path()).map(|i| i.id()),
                Some(icon.id())
            );
            assert!(
                icon.bytes().starts_with(b"\x89PNG\r\n\x1a\n"),
                "运行时图标应为内嵌 PNG：{}",
                icon.path()
            );
        }
        assert_eq!(
            MaterialIcon::from_path("icons/material/not-found.png").map(|i| i.id()),
            None
        );
    }

    /// 验证映射表与协议图标引用的所有图标都已内嵌。
    #[test]
    fn all_mapping_icons_are_bundled() {
        let mut ids = HashSet::new();
        for (_, id) in FILE_NAME_ICONS {
            ids.insert(*id);
        }
        for (_, id) in FILE_EXTENSION_ICONS {
            ids.insert(*id);
        }
        for id in ["file", "folder-base", "folder-base-open"] {
            ids.insert(id);
        }
        for icon in [ssh_icon(), smb_icon(), git_icon(), svn_icon()] {
            ids.insert(icon.id());
        }
        for id in ids {
            assert!(
                MaterialIcon::by_id(id).is_some(),
                "映射引用了未内嵌的图标：{id}"
            );
        }
    }

    /// 验证文件解析优先精确名、再日志/压缩包特判，最后按最长扩展名回退。
    #[test]
    fn resolves_file_icons_like_material_icon_theme() {
        assert_eq!(file_icon_for_name("sys.log").id(), "log");
        assert_eq!(file_icon_for_name("sys.log.1").id(), "log");
        assert_eq!(file_icon_for_name("sys.log-2026-10-27").id(), "log");
        assert_eq!(file_icon_for_name("logs.tar.gz").id(), "zip");
        assert_eq!(file_icon_for_name("dump.tar.gz").id(), "zip");
        assert_eq!(file_icon_for_name("env.properties").id(), "settings");
        assert_eq!(file_icon_for_name("server.xml").id(), "xml");
        assert_eq!(file_icon_for_name("config.yaml").id(), "yaml");
        assert_eq!(file_icon_for_name("Dockerfile").id(), "docker");
        assert_eq!(file_icon_for_name("pom.xml").id(), "maven");
        assert_eq!(file_icon_for_name("notes.unknown").id(), "file");
        assert_eq!(file_icon_for_name("README.md").id(), "readme");
    }

    /// 验证目录统一使用第一版棕色文件夹，连接树协议图标各有独立配色。
    #[test]
    fn uses_brown_folders_and_distinct_protocol_icons() {
        assert_eq!(folder_icon(false).id(), "folder-base");
        assert_eq!(folder_icon(true).id(), "folder-base-open");
        assert_eq!(ssh_icon().id(), "console");
        assert_eq!(smb_icon().id(), "database");
        assert_eq!(git_icon().id(), "git");
        assert_eq!(svn_icon().id(), "changelog");
    }
}
