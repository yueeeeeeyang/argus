//! 文件职责：定义 Argus 界面使用的 Lucide 图标清单与 SVG 渲染入口。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：将稳定的业务语义图标映射到 icondata Lucide 图标常量。

use gpui::{IntoElement, prelude::*, px, rgb, svg};
use icondata::Icon;

/// Argus UI 中允许使用的图标语义枚举。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArgusIcon {
    /// 日志分析工作区。
    Logs,
    /// 设置工作区。
    Settings,
    /// 关于或信息。
    Info,
    /// 打开来源。
    Open,
    /// 加载日志来源。
    FolderPlus,
    /// 全局搜索或搜索框。
    Search,
    /// 来源树过滤。
    Filter,
    /// 连接入口。
    Connection,
    /// 新增来源或增大数值。
    Plus,
    /// 减少数值。
    Minus,
    /// 关闭标签或弹窗。
    Close,
    /// 布局切换。
    Layout,
    /// 更多操作。
    More,
    /// 后退。
    ArrowLeft,
    /// 向上跳转。
    ArrowUp,
    /// 前进。
    ArrowRight,
    /// 向下跳转。
    ArrowDown,
    /// 刷新。
    Refresh,
    /// 折叠或向上收起。
    Collapse,
    /// 全部收起目录树。
    ListCollapse,
    /// 展开。
    Expand,
    /// 自动换行。
    Wrap,
    /// 目录节点。
    Folder,
    /// 已展开目录节点。
    FolderOpen,
    /// 普通文件节点。
    File,
    /// 日志文件节点。
    FileText,
    /// 压缩包节点。
    Archive,
    /// 主题设置。
    Palette,
    /// 编码设置。
    Type,
    /// 缓存设置。
    Database,
    /// 快捷键设置。
    Keyboard,
    /// 大小写匹配。
    CaseSensitive,
    /// 正则搜索。
    Regex,
    /// 全词匹配。
    WholeWord,
    /// 开关关闭。
    ToggleLeft,
    /// 开关开启。
    ToggleRight,
}

impl ArgusIcon {
    /// 返回所有可由内存资产源加载的图标。
    pub fn all() -> &'static [Self] {
        &[
            Self::Logs,
            Self::Settings,
            Self::Info,
            Self::Open,
            Self::FolderPlus,
            Self::Search,
            Self::Filter,
            Self::Connection,
            Self::Plus,
            Self::Minus,
            Self::Close,
            Self::Layout,
            Self::More,
            Self::ArrowLeft,
            Self::ArrowUp,
            Self::ArrowRight,
            Self::ArrowDown,
            Self::Refresh,
            Self::Collapse,
            Self::ListCollapse,
            Self::Expand,
            Self::Wrap,
            Self::Folder,
            Self::FolderOpen,
            Self::File,
            Self::FileText,
            Self::Archive,
            Self::Palette,
            Self::Type,
            Self::Database,
            Self::Keyboard,
            Self::CaseSensitive,
            Self::Regex,
            Self::WholeWord,
            Self::ToggleLeft,
            Self::ToggleRight,
        ]
    }

    /// 根据 GPUI 请求路径反查图标。
    pub fn from_path(path: &str) -> Option<Self> {
        Self::all().iter().copied().find(|icon| icon.path() == path)
    }

    /// 返回 GPUI SVG 元素使用的资产路径。
    pub fn path(self) -> &'static str {
        match self {
            Self::Logs => "icons/logs.svg",
            Self::Settings => "icons/settings.svg",
            Self::Info => "icons/info.svg",
            Self::Open => "icons/open.svg",
            Self::FolderPlus => "icons/folder-plus.svg",
            Self::Search => "icons/search.svg",
            Self::Filter => "icons/filter.svg",
            Self::Connection => "icons/connection.svg",
            Self::Plus => "icons/plus.svg",
            Self::Minus => "icons/minus.svg",
            Self::Close => "icons/close.svg",
            Self::Layout => "icons/layout.svg",
            Self::More => "icons/more.svg",
            Self::ArrowLeft => "icons/arrow-left.svg",
            Self::ArrowUp => "icons/arrow-up.svg",
            Self::ArrowRight => "icons/arrow-right.svg",
            Self::ArrowDown => "icons/arrow-down.svg",
            Self::Refresh => "icons/refresh.svg",
            Self::Collapse => "icons/collapse.svg",
            Self::ListCollapse => "icons/list-collapse.svg",
            Self::Expand => "icons/expand.svg",
            Self::Wrap => "icons/wrap.svg",
            Self::Folder => "icons/folder.svg",
            Self::FolderOpen => "icons/folder-open.svg",
            Self::File => "icons/file.svg",
            Self::FileText => "icons/file-text.svg",
            Self::Archive => "icons/archive.svg",
            Self::Palette => "icons/palette.svg",
            Self::Type => "icons/type.svg",
            Self::Database => "icons/database.svg",
            Self::Keyboard => "icons/keyboard.svg",
            Self::CaseSensitive => "icons/case-sensitive.svg",
            Self::Regex => "icons/regex.svg",
            Self::WholeWord => "icons/whole-word.svg",
            Self::ToggleLeft => "icons/toggle-left.svg",
            Self::ToggleRight => "icons/toggle-right.svg",
        }
    }

    /// 返回资产目录列表使用的文件名。
    pub fn file_name(self) -> &'static str {
        self.path().trim_start_matches("icons/")
    }

    /// 将 icondata 的路径片段包装为完整 SVG 文档。
    pub fn to_svg_string(self) -> String {
        let icon = self.icon_data();
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="{view_box}" fill="{fill}" stroke="{stroke}" stroke-width="{stroke_width}" stroke-linecap="{stroke_linecap}" stroke-linejoin="{stroke_linejoin}">{data}</svg>"#,
            width = icon.width.unwrap_or("24"),
            height = icon.height.unwrap_or("24"),
            view_box = icon.view_box.unwrap_or("0 0 24 24"),
            fill = icon.fill.unwrap_or("none"),
            stroke = icon.stroke.unwrap_or("currentColor"),
            stroke_width = icon.stroke_width.unwrap_or("2"),
            stroke_linecap = icon.stroke_linecap.unwrap_or("round"),
            stroke_linejoin = icon.stroke_linejoin.unwrap_or("round"),
            data = icon.data,
        )
    }

    /// 返回 Lucide 图标数据常量。
    fn icon_data(self) -> Icon {
        match self {
            Self::Logs => icondata::LuLogs,
            Self::Settings => icondata::LuSettings,
            Self::Info => icondata::LuInfo,
            Self::Open => icondata::LuFolderOpen,
            Self::FolderPlus => icondata::LuFolderPlus,
            Self::Search => icondata::LuSearch,
            Self::Filter => icondata::LuListFilter,
            Self::Connection => icondata::LuPlug,
            Self::Plus => icondata::LuPlus,
            Self::Minus => icondata::LuMinus,
            Self::Close => icondata::LuX,
            Self::Layout => icondata::LuPanelLeft,
            Self::More => icondata::LuEllipsis,
            Self::ArrowLeft => icondata::LuArrowLeft,
            Self::ArrowUp => icondata::LuArrowUp,
            Self::ArrowRight => icondata::LuArrowRight,
            Self::ArrowDown => icondata::LuArrowDown,
            Self::Refresh => icondata::LuRefreshCw,
            Self::Collapse => icondata::LuChevronDown,
            Self::ListCollapse => icondata::LuListCollapse,
            Self::Expand => icondata::LuChevronRight,
            Self::Wrap => icondata::LuWrapText,
            Self::Folder => icondata::LuFolder,
            Self::FolderOpen => icondata::LuFolderOpen,
            Self::File => icondata::LuFile,
            Self::FileText => icondata::LuFileText,
            Self::Archive => icondata::LuArchive,
            Self::Palette => icondata::LuPalette,
            Self::Type => icondata::LuType,
            Self::Database => icondata::LuDatabase,
            Self::Keyboard => icondata::LuKeyboard,
            Self::CaseSensitive => icondata::LuCaseSensitive,
            Self::Regex => icondata::LuRegex,
            Self::WholeWord => icondata::LuWholeWord,
            Self::ToggleLeft => icondata::LuToggleLeft,
            Self::ToggleRight => icondata::LuToggleRight,
        }
    }
}

/// 渲染一个继承文本颜色的 Lucide SVG 图标。
///
/// 参数说明：
/// - `icon`：图标语义。
/// - `color`：当前图标颜色。
/// - `size`：图标边长，单位为逻辑像素。
///
/// 返回值：GPUI SVG 元素；图标内容由内存资产源提供。
pub fn render_icon(icon: ArgusIcon, color: u32, size: f32) -> impl IntoElement {
    svg()
        .path(icon.path())
        .size(px(size))
        .text_color(rgb(color))
}
