//! 文件职责：集中维护应用范围内的常量定义。
//! 创建日期：2026-07-08
//! 作者：Argus 开发团队
//! 主要功能：提供侧栏宽度、日志正文字号、搜索结果面板尺寸、行号栏布局、Jstack 线程详情窗口、
//!           远程文件预览窗口和 Runtime SQL 明细行高等常量，供界面渲染与命中测试统一引用。

use crate::ui::custom_title_bar::TITLE_BAR_HEIGHT;

/// 来源侧栏默认宽度；主窗口默认宽度同步增加，避免挤占右侧日志阅读区。
pub const SOURCE_PANEL_DEFAULT_WIDTH: f32 = 350.0;
/// 来源侧栏最小宽度，需保证标题栏左侧 4 个操作按钮和固定右侧间距完整展示。
pub const SOURCE_PANEL_MIN_WIDTH: f32 = 244.0;
/// 来源侧栏最大宽度，避免占位界面被侧栏挤压。
pub const SOURCE_PANEL_MAX_WIDTH: f32 = 520.0;
/// 日志内容字号最小值，避免主阅读区文字过小影响可读性。
pub const LOG_CONTENT_FONT_SIZE_MIN: f32 = 12.0;
/// 日志内容字号最大值，避免大字号破坏当前日志行布局。
pub const LOG_CONTENT_FONT_SIZE_MAX: f32 = 20.0;
/// 日志内容默认字号，匹配设计文档要求的高密度 12px 阅读区。
pub const LOG_CONTENT_FONT_SIZE_DEFAULT: f32 = 12.0;
/// 搜索结果面板默认高度。
pub const SEARCH_RESULT_PANEL_HEIGHT_DEFAULT: f32 = 220.0;
/// 搜索结果面板最小高度，保证标题和至少几行结果可见。
pub const SEARCH_RESULT_PANEL_HEIGHT_MIN: f32 = 140.0;
/// 搜索结果面板最大高度兜底值，主要用于单元测试验证 clamp 行为；运行时实际上限随窗口高度动态计算。
pub const SEARCH_RESULT_PANEL_HEIGHT_MAX: f32 = 520.0;
/// 搜索结果面板拖拽到最大高度时，为上方日志内容保留的最小可见高度。
///
/// 与自定义标题栏高度无关：仅约束日志正文最小可见区，确保面板近乎撑满时仍能看到几行日志。
pub const SEARCH_RESULT_PANEL_MIN_LOG_VIEW_HEIGHT: f32 = 60.0;
/// 搜索结果面板拖拽时为上方日志内容保留的最小高度（含自定义标题栏与最小日志可见区），
/// 面板最大可拖至窗口视口高度减去此值，使其近乎撑满整个窗口。
///
/// 由 `TITLE_BAR_HEIGHT` 与 `SEARCH_RESULT_PANEL_MIN_LOG_VIEW_HEIGHT` 派生，
/// 标题栏高度调整时无需同步修改此处的字面量。
pub const SEARCH_RESULT_PANEL_RESERVED_HEIGHT: f32 =
    TITLE_BAR_HEIGHT + SEARCH_RESULT_PANEL_MIN_LOG_VIEW_HEIGHT;
/// 日志正文左侧内边距；命中测试和渲染必须保持一致。
pub const LOG_VIEWER_TEXT_LEFT_PADDING: f32 = 16.0;
/// 日志正文右侧内边距；横向滚动范围和渲染必须保持一致。
pub const LOG_VIEWER_TEXT_RIGHT_PADDING: f32 = 16.0;
/// Jstack 线程详情窗口默认宽度。
pub const JSTACK_THREAD_DETAIL_WINDOW_WIDTH: f32 = 900.0;
/// Jstack 线程详情窗口默认高度。
pub const JSTACK_THREAD_DETAIL_WINDOW_HEIGHT: f32 = 640.0;
/// Jstack 线程详情窗口最小宽度。
pub const JSTACK_THREAD_DETAIL_WINDOW_MIN_WIDTH: f32 = 600.0;
/// Jstack 线程详情窗口最小高度。
pub const JSTACK_THREAD_DETAIL_WINDOW_MIN_HEIGHT: f32 = 420.0;
/// 远程文件预览窗口默认宽度。
pub const FILE_PREVIEW_WINDOW_WIDTH: f32 = 920.0;
/// 远程文件预览窗口默认高度。
pub const FILE_PREVIEW_WINDOW_HEIGHT: f32 = 640.0;
/// 远程文件预览窗口最小宽度。
pub const FILE_PREVIEW_WINDOW_MIN_WIDTH: f32 = 600.0;
/// 远程文件预览窗口最小高度。
pub const FILE_PREVIEW_WINDOW_MIN_HEIGHT: f32 = 420.0;
/// 日志正文固定行高；分页滚动和 UI 渲染必须保持一致。
pub const LOG_VIEWER_ROW_HEIGHT: f32 = 20.0;
/// 行号栏最小宽度，保证小文件也有稳定的视觉留白。
pub const LOG_VIEWER_LINE_NUMBER_MIN_WIDTH: f32 = 44.0;
/// 行号栏最大宽度，避免超大文件行号挤占正文区域。
pub const LOG_VIEWER_LINE_NUMBER_MAX_WIDTH: f32 = 96.0;
/// 行号栏单个数字的估算宽度，用于无布局测量时的稳定宽度计算。
pub const LOG_VIEWER_LINE_NUMBER_DIGIT_WIDTH: f32 = 7.0;
/// 行号栏左右留白总和，保证行号和正文之间有清晰间隔。
pub const LOG_VIEWER_LINE_NUMBER_PADDING: f32 = 18.0;
/// 日志正文中的制表符展示为空格时的固定宽度。
pub const LOG_VIEWER_TAB_DISPLAY_SPACES: &str = "    ";
/// 后台压缩包探测每批最多处理 `并发数 * 该系数` 个节点，避免频繁重绘。
pub const SOURCE_ARCHIVE_PROBE_BATCH_FACTOR: usize = 16;
/// Runtime 过滤输入防抖时长，避免每个字符都触发大结果集重新过滤。
pub const RUNTIME_FILTER_DEBOUNCE_MS: u64 = 260;

/// Runtime SQL 明细收起态固定行高。
///
/// 说明：Runtime SQL 表格已固定为单行展示，长 SQL 由单元格内部横向滚动承载；
/// UI 层的 SQL 行高必须与这里保持一致，才能让虚拟列表滚动条稳定计算范围。
pub const RUNTIME_SQL_COLLAPSED_ROW_HEIGHT: f32 = 36.0;
