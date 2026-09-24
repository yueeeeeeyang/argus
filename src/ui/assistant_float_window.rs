//! 文件职责：渲染日志助手浮动独立窗口。
//! 创建日期：2026-09-24
//! 修改日期：2026-09-24
//! 作者：Argus 开发团队
//! 主要功能：在主窗口右侧空间充足时以独立浮动窗口承载日志助手面板，窗口骨架与文件预览窗口一致。

#[cfg(not(target_os = "windows"))]
use gpui::WindowControlArea;
use gpui::{
    Context, Entity, FocusHandle, FontWeight, IntoElement, MouseButton, MouseDownEvent, Render,
    Subscription, Window, div, prelude::*, px, rgb,
};

use crate::app::{ArgusApp, observe_app_theme};
use crate::fonts::ARGUS_UI_FONT_FAMILY;
use crate::platform::custom_titlebar;
use crate::theme::AppTheme;
use crate::ui::assistant_panel::AssistantPanel;
use crate::ui::custom_title_bar::{TITLE_BAR_HEIGHT, platform_window_controls};
use crate::ui::main_window::{
    WINDOW_CONTENT_INSET, WINDOW_CONTENT_PADDING, WINDOW_CONTENT_RADIUS,
    WINDOW_CONTENT_RING_ALLOWANCE, window_content_shadows,
};

/// 原生 resize 写回面板宽度偏好的最小变化阈值（像素），避免逐帧抖动写。
const ASSISTANT_FLOAT_WIDTH_WRITE_BACK_EPSILON: f32 = 0.5;

/// 日志助手浮动独立窗口视图。
pub(crate) struct AssistantFloatWindow {
    /// 主应用实体，用于宽度偏好写回和关闭时状态清理。
    app: Entity<ArgusApp>,
    /// 内嵌助手面板实体；浮动与内嵌共用同一会话。
    panel: Entity<AssistantPanel>,
    /// 当前窗口使用的主题快照。
    theme: AppTheme,
    /// 窗口根元素焦点句柄，用于接收键盘事件并稳定焦点归属。
    root_focus: FocusHandle,
    /// 是否已注册窗口关闭钩子，避免每帧重复注册。
    has_registered_close_guard: bool,
    /// 最近一次写回应用的视口宽度；渲染期禁止读写主应用实体，改用本地值比较。
    last_synced_width: f32,
    /// 主应用状态订阅，主题切换后窗口跟随刷新。
    _app_observer: Subscription,
}

impl AssistantFloatWindow {
    /// 创建日志助手浮动窗口。
    ///
    /// 参数说明：
    /// - `app`：主应用实体。
    /// - `panel`：已创建的助手面板实体，浮动窗口直接承载其渲染。
    /// - `theme`：首次绘制使用的主题。
    /// - `initial_width`：窗口创建帧宽度，作为宽度写回的本地比较基准。
    /// - `cx`：窗口上下文，用于创建焦点句柄和订阅主应用变化。
    pub(crate) fn new(
        app: Entity<ArgusApp>,
        panel: Entity<AssistantPanel>,
        theme: AppTheme,
        initial_width: f32,
        cx: &mut Context<Self>,
    ) -> Self {
        let _app_observer = observe_app_theme(cx, &app, theme.clone(), |view, theme, _| {
            view.theme = theme.clone();
        });

        Self {
            app,
            panel,
            theme,
            root_focus: cx.focus_handle(),
            has_registered_close_guard: false,
            last_synced_width: initial_width,
            _app_observer,
        }
    }

    /// 注册窗口关闭钩子：用户点红绿灯关闭时同步收起助手状态，返回 `true` 放行系统关闭。
    fn ensure_close_guard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.has_registered_close_guard {
            return;
        }
        let app = self.app.clone();
        window.on_window_should_close(cx, move |float_window, app_cx| {
            crate::platform::custom_titlebar::clear_repeated_click_guard(float_window);
            app.update(app_cx, |app, _| {
                app.close_assistant_float_window(Some("已收起日志助手"), false);
            });
            true
        });
        self.has_registered_close_guard = true;
    }

    /// 用户拖动窗口边缘改变宽度时，把新宽度写回助手面板宽度偏好。
    ///
    /// 说明：渲染期间主应用实体可能被主窗口渲染或其他更新借用，直接读写会触发
    /// GPUI 实体重入 panic；这里只与本地缓存比较，变化超过阈值时延迟到事件循环空闲时写回。
    /// 主窗口的跟随同步以帧缓存相等为条件，写回不会形成循环。
    fn write_back_panel_width(&mut self, window: &Window, cx: &mut Context<Self>) {
        let viewport_width = window.viewport_size().width / px(1.0);
        if viewport_width <= 0.0
            || (self.last_synced_width - viewport_width).abs()
                <= ASSISTANT_FLOAT_WIDTH_WRITE_BACK_EPSILON
        {
            return;
        }
        self.last_synced_width = viewport_width;
        let app = self.app.clone();
        cx.defer(move |cx| {
            app.update(cx, |app, _| {
                app.assistant_panel_width = viewport_width;
            });
        });
    }

    /// 渲染浮动窗口标题栏：平台窗口控件（macOS 原生红绿灯占位）、标题文本、拖拽空白。
    ///
    /// 关闭/最小化/最大化由系统红绿灯承担，不提供自定义关闭按钮；
    /// 标题栏骨架（高度、底色、拖拽与双击缩放）与主窗口自定义标题栏、文件预览窗口保持一致。
    fn render_header(&self, window: &Window) -> impl IntoElement {
        let theme = self.theme.clone();
        let is_maximized = window.is_maximized();

        div()
            .id("assistant-float-title-bar")
            .h(px(TITLE_BAR_HEIGHT))
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .bg(rgb(theme.title_bar))
            .occlude()
            .child(platform_window_controls(is_maximized, &theme))
            .child(
                // 与主标题栏按钮组保持同一节奏，避开红绿灯右缘。
                div()
                    .pl(px(8.0))
                    .flex_none()
                    .text_size(px(13.0))
                    .line_height(px(18.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(theme.foreground))
                    .child("日志助手"),
            )
            .child(float_title_drag_area())
    }
}

impl Render for AssistantFloatWindow {
    /// 渲染浮动助手窗口主体：黑色背板 + 与主窗口一致的圆角玻璃板内容区。
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_close_guard(window, cx);
        self.write_back_panel_width(window, cx);
        let root_focus_for_click = self.root_focus.clone();
        div()
            .id("assistant-float-window-root")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(rgb(self.theme.background))
            .font_family(ARGUS_UI_FONT_FAMILY)
            .text_color(rgb(self.theme.foreground))
            .occlude()
            .focusable()
            .track_focus(&self.root_focus)
            .on_click(move |_, window, _| {
                root_focus_for_click.focus(window);
            })
            .child(self.render_header(window))
            .child(
                // 与文件预览窗口同一套玻璃板结构：8px 窗口间距 + 圆角底板 + 多层投影，
                // 内部 8px 留白保证内容不盖住圆角（GPUI 裁剪只支持矩形）。
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .pt(px(WINDOW_CONTENT_RING_ALLOWANCE))
                    .pl(px(WINDOW_CONTENT_INSET))
                    .pr(px(WINDOW_CONTENT_INSET))
                    // 面板底边与窗口下缘之间收窄到 6px 窗口间距。
                    .pb(px(6.0))
                    .child(
                        div()
                            .size_full()
                            .p(px(WINDOW_CONTENT_PADDING))
                            .rounded(px(WINDOW_CONTENT_RADIUS))
                            .bg(rgb(self.theme.content))
                            .shadow(window_content_shadows())
                            .child(
                                div()
                                    .size_full()
                                    .flex()
                                    .flex_col()
                                    .overflow_hidden()
                                    .child(div().flex_1().min_h(px(0.0)).child(self.panel.clone())),
                            ),
                    ),
            )
    }
}

/// 渲染浮动窗口标题栏的拖拽空白，支持拖动窗口与双击最大化；范式同文件预览窗口拖拽区。
fn float_title_drag_area() -> impl IntoElement {
    let drag_area = div()
        .id("assistant-float-title-drag-area")
        .h_full()
        .flex_1();
    // Windows 需要普通客户区事件调用显式窗口操作；其余平台保留 GPUI 控制区语义。
    #[cfg(not(target_os = "windows"))]
    let drag_area = drag_area.window_control_area(WindowControlArea::Drag);

    drag_area.on_mouse_down(
        MouseButton::Left,
        move |event: &MouseDownEvent, window, cx| {
            match event.click_count {
                1 => custom_titlebar::start_window_drag(window),
                2 => window.zoom_window(),
                _ => {}
            }
            cx.stop_propagation();
        },
    )
}
