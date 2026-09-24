//! 文件职责：组合 Argus 主窗口的整体布局。
//! 创建日期：2026-06-09
//! 修改日期：2026-09-24
//! 作者：Argus 开发团队
//! 主要功能：渲染标题栏、来源侧栏、日志内容区、右侧 Agent 助手、AI 分析弹窗和设置模态框。

use crate::app::ArgusApp;
use crate::fonts::ARGUS_UI_FONT_FAMILY;
use crate::infra::perf::PerfSpan;
use crate::ui::{
    agent_dialog, ai_settings_editor, archive_password_dialog, components::context_menu,
    connection_dialog, custom_title_bar, log_content_view, log_search_dialog, remote_file_dialog,
    settings_window, source_panel, source_picker, source_resizer,
};
use gpui::{
    Animation, AnimationExt, AnyElement, BoxShadow, ClickEvent, Context, ExternalPaths,
    IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Window, div, hsla,
    point, prelude::*, px, rgb,
};
use std::time::Duration;

/// 内容玻璃板与窗口边框/侧栏之间的间距（像素）。
pub(crate) const WINDOW_CONTENT_INSET: f32 = 8.0;
/// 内容玻璃板圆角半径（像素）。
pub(crate) const WINDOW_CONTENT_RADIUS: f32 = 6.0;
/// 内容玻璃板外描边在板面之外 0.5px，而父内容行容器会按自身边界裁剪，
/// 因此顶边需要留出等宽余量，否则上边框亮线会被切掉（其余三边已有 8px 间距）。
pub(crate) const WINDOW_CONTENT_RING_ALLOWANCE: f32 = 0.5;
/// 玻璃板内部留白（像素）；保持与外侧一致的 8px 视觉节奏，且不小于圆角半径，
/// 保证内容不会盖住圆角。
///
/// GPUI 的内容裁剪只支持矩形，圆角只能靠"子元素够不到四角"来保持可见。
pub(crate) const WINDOW_CONTENT_PADDING: f32 = 8.0;
/// 编译期守住"内部留白 ≥ 圆角半径"：GPUI 无法裁剪圆角，比例失守就会露馅。
const _: () = assert!(WINDOW_CONTENT_PADDING >= WINDOW_CONTENT_RADIUS);
/// 玻璃板主投影向下偏移（像素）。
const WINDOW_CONTENT_SHADOW_PRIMARY_OFFSET_Y: f32 = 2.0;
/// 玻璃板主投影模糊半径（像素）。
const WINDOW_CONTENT_SHADOW_PRIMARY_BLUR: f32 = 4.0;
/// 玻璃板次投影向下偏移（像素）；贴近板面，补足主投影近处的过渡。
const WINDOW_CONTENT_SHADOW_SECONDARY_OFFSET_Y: f32 = 1.0;
/// 玻璃板次投影模糊半径（像素）。
const WINDOW_CONTENT_SHADOW_SECONDARY_BLUR: f32 = 2.0;
/// 玻璃板投影不透明度；投影使用中性黑，与主题配色无关。
const WINDOW_CONTENT_SHADOW_OPACITY: f32 = 0.30;
/// 玻璃板边缘描边宽度（像素）；用向外扩散、零模糊的投影实现外描边。
const WINDOW_CONTENT_EDGE_WIDTH: f32 = 0.5;
/// 玻璃板边缘描边不透明度；描边使用中性白，在深色背景上勾出板面轮廓。
const WINDOW_CONTENT_EDGE_OPACITY: f32 = 0.16;
/// 玻璃板顶部高光向上偏移（像素）；用零模糊投影在板面顶边形成一条亮线。
const WINDOW_CONTENT_TOP_HIGHLIGHT_OFFSET_Y: f32 = -0.5;
/// 玻璃板顶部高光不透明度；高光使用中性白，模拟玻璃边缘受光。
const WINDOW_CONTENT_TOP_HIGHLIGHT_OPACITY: f32 = 0.06;

/// 构造内容玻璃板的凸起阴影层。
///
/// 数值参考 opencode v2 客户端内容面板的 `--v2-elevation-raised`（深色主题取值）：
/// 两层向下投影把板面从窗口背景上抬起，0.5px 白色外描边勾出板面轮廓，
/// 顶部 0.5px 白色亮线模拟玻璃边缘受光。阴影颜色为中性黑/白，不依赖主题配色；
/// 顺序与 CSS 阴影列表一致，越靠后绘制的层级越靠上。
pub(crate) fn window_content_shadows() -> Vec<BoxShadow> {
    let shadow_color = hsla(0.0, 0.0, 0.0, WINDOW_CONTENT_SHADOW_OPACITY);
    let edge_color = hsla(0.0, 0.0, 1.0, WINDOW_CONTENT_EDGE_OPACITY);
    let top_highlight_color = hsla(0.0, 0.0, 1.0, WINDOW_CONTENT_TOP_HIGHLIGHT_OPACITY);

    vec![
        BoxShadow {
            color: shadow_color,
            offset: point(px(0.0), px(WINDOW_CONTENT_SHADOW_PRIMARY_OFFSET_Y)),
            blur_radius: px(WINDOW_CONTENT_SHADOW_PRIMARY_BLUR),
            spread_radius: px(0.0),
        },
        BoxShadow {
            color: shadow_color,
            offset: point(px(0.0), px(WINDOW_CONTENT_SHADOW_SECONDARY_OFFSET_Y)),
            blur_radius: px(WINDOW_CONTENT_SHADOW_SECONDARY_BLUR),
            spread_radius: px(0.0),
        },
        BoxShadow {
            color: edge_color,
            offset: point(px(0.0), px(0.0)),
            blur_radius: px(0.0),
            spread_radius: px(WINDOW_CONTENT_EDGE_WIDTH),
        },
        BoxShadow {
            color: top_highlight_color,
            offset: point(px(0.0), px(WINDOW_CONTENT_TOP_HIGHLIGHT_OFFSET_Y)),
            blur_radius: px(0.0),
            spread_radius: px(0.0),
        },
    ]
}

/// 渲染 Argus 根布局。
///
/// 参数说明：
/// - `app`：应用状态，包含当前工作区和占位数据。
/// - `window`：GPUI 窗口对象，用于自定义窗口按钮。
/// - `cx`：应用上下文，用于为子组件创建状态更新回调。
///
/// 返回值：GPUI 元素树；当前不会抛出业务异常。
pub(crate) fn render(
    app: &mut ArgusApp,
    window: &mut Window,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let _span = PerfSpan::new("main_window_render");
    app.sync_window_appearance_theme(window);
    if !app.has_registered_workspace_close_guard {
        // 主窗口关闭时删除当前物化工作目录；崩溃或强杀的场景由下次启动清扫兜底。
        let entity = cx.entity();
        window.on_window_should_close(cx, move |_, app_cx| {
            if let Some(root) = entity.read(app_cx).source_workspace_root.clone() {
                crate::loader::workspace::delete_workspace_best_effort(root);
            }
            true
        });
        app.has_registered_workspace_close_guard = true;
    }
    let input_focus_handles = app.ensure_input_focus_handles(cx);
    let root_focus_for_track = input_focus_handles.root.clone();
    let root_focus_for_click = input_focus_handles.root.clone();
    let theme = app.theme.clone();

    div()
        .id("argus-root")
        .relative()
        .size_full()
        .flex()
        .flex_col()
        .bg(rgb(theme.background))
        .font_family(ARGUS_UI_FONT_FAMILY)
        .text_color(rgb(theme.foreground))
        .focusable()
        .track_focus(&root_focus_for_track)
        .on_click(cx.listener(move |app, _event: &ClickEvent, window, cx| {
            root_focus_for_click.focus(window);
            app.clear_all_text_input_focus();
            cx.notify();
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|app, _: &MouseDownEvent, _, cx| {
                if app.clear_runtime_cell_selection() {
                    cx.notify();
                }
            }),
        )
        .on_mouse_move(cx.listener(|app, event: &MouseMoveEvent, _window, cx| {
            let pointer_x = event.position.x / px(1.0);
            if app.resize_panels_from_pointer(pointer_x, _window) {
                cx.notify();
            }
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|app, _: &MouseUpEvent, _, cx| {
                if app.finish_panel_resizes() {
                    cx.notify();
                }
            }),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(|app, _: &MouseUpEvent, _, cx| {
                if app.finish_panel_resizes() {
                    cx.notify();
                }
            }),
        )
        .on_drop(cx.listener(|app, paths: &ExternalPaths, _, cx| {
            // 系统文件拖拽会被 GPUI 包装成 `ExternalPaths`，这里只负责接收事件；
            // 文件、目录、压缩包是否可读仍由统一的来源加载器判断并反馈。
            if app.load_dropped_sources(paths.paths(), cx) {
                cx.notify();
            }
        }))
        .child(custom_title_bar::render(app, window, cx))
        .child(
            div()
                .flex()
                .flex_1()
                .overflow_hidden()
                .bg(rgb(theme.background))
                .child(animated_source_panel(app, cx))
                // 内容区以玻璃板形态浮在窗口背景上，分两级：
                // 1）透明留白容器负责与窗口左右边框、底边和侧栏之间 8px 的窗口背景间距；
                //    顶边只留 0.5px 外描边余量——标题栏（44px）已包含标签下方的 8px 留白，
                //    与 opencode v2 一致，同时保证上边框亮线不被裁剪；
                // 2）玻璃板自身绘制圆角底色，并用内部留白把内容挡在四角之外
                //    （GPUI 的内容裁剪只支持矩形，子元素会盖住圆角）。
                .child(
                    div()
                        .flex_1()
                        .h_full()
                        .pt(px(WINDOW_CONTENT_RING_ALLOWANCE))
                        .pl(px(WINDOW_CONTENT_INSET))
                        .pr(px(WINDOW_CONTENT_INSET))
                        .pb(px(WINDOW_CONTENT_INSET))
                        .child(
                            div()
                                .size_full()
                                .rounded(px(WINDOW_CONTENT_RADIUS))
                                .bg(rgb(theme.content))
                                // 凸起质感：多层阴影把板面从窗口背景上抬起，并用 0.5px 亮色
                                // 外描边与顶部亮线勾出玻璃边缘（见 `window_content_shadows`）。
                                .shadow(window_content_shadows())
                                .debug_selector(|| "window-content-panel".to_string())
                                .flex()
                                .flex_col()
                                // 主内容区保留 8px 内部留白，保证内容不盖住玻璃板圆角。
                                .child(
                                    div()
                                        .flex_1()
                                        .min_h(px(0.0))
                                        .p(px(WINDOW_CONTENT_PADDING))
                                        .child(log_content_view::render(app, window, cx)),
                                )
                                // 搜索结果面板与内容面板完全贴合：不再保留玻璃板内边距，
                                // 面板底角圆角与玻璃板一致。
                                .when(app.should_show_log_search_results(), |this| {
                                    this.child(log_content_view::render_search_results_panel(
                                        app, &theme, cx,
                                    ))
                                }),
                        ),
                )
                .child(animated_assistant_panel(app, window, cx)),
        )
        .when(!app.is_source_panel_collapsed, |this| {
            this.child(source_resizer::render(app, "source-resizer", cx))
        })
        .when(app.connection_dialog.is_some(), |this| {
            this.child(connection_dialog::render(app, cx))
        })
        .when(app.remote_file_dialog.is_some(), |this| {
            this.child(remote_file_dialog::render(app, cx))
        })
        // 搜索对话框先于密码弹窗等更高优先级提示渲染，保证搜索加密来源时密码弹窗显示在最上层。
        .when_some(app.log_search.search_view.clone(), |this, search_view| {
            this.child(log_search_dialog::render_log_search_dialog(
                search_view,
                &theme,
                cx,
            ))
        })
        .when(app.archive_password_prompt.is_some(), |this| {
            this.child(archive_password_dialog::render(app, cx))
        })
        .when(app.active_menu.is_some(), |this| {
            this.child(context_menu::render_active_menu(app, cx))
        })
        .when_some(app.source_picker_modal.clone(), |this, modal| {
            this.child(source_picker::render_source_picker_modal(modal, &theme, cx))
        })
        .when_some(app.ai_agent_launch_modal.clone(), |this, modal| {
            this.child(agent_dialog::render_agent_launch_modal(modal, &theme, cx))
        })
        .when_some(app.connection_directory_modal.clone(), |this, modal| {
            this.child(connection_dialog::render_connection_directory_modal(
                modal, &theme, cx,
            ))
        })
        .when_some(app.connection_link_modal.clone(), |this, modal| {
            this.child(connection_dialog::render_connection_link_modal(
                modal, &theme, cx,
            ))
        })
        .when(app.is_settings_modal_open, |this| {
            this.child(settings_window::render_settings_modal(
                app,
                &input_focus_handles,
                cx,
            ))
        })
        .when_some(app.ai_settings_editor_modal.clone(), |this, modal| {
            this.child(ai_settings_editor::render_ai_settings_editor_modal(
                modal, &theme, cx,
            ))
        })
}

/// 渲染主内容右侧可动画的 Agent 助手容器；实体在首次展开后持续保留。
fn animated_assistant_panel(
    app: &ArgusApp,
    window: &Window,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    let window_width = window.viewport_size().width / px(1.0);
    let visible_width = app.current_assistant_panel_width_for_window(window_width);
    let dynamic_max = app.assistant_panel_max_width_for_window(window_width);
    let from_width = app.assistant_panel_animation_from_width.min(dynamic_max);
    let to_width = app.assistant_panel_animation_to_width.min(dynamic_max);
    let panel = div()
        .id("animated-assistant-panel")
        .relative()
        .h_full()
        .flex_none()
        .overflow_hidden()
        .flex()
        .when_some(app.assistant_panel.clone(), |this, panel| {
            this.child(div().flex_1().min_w(px(0.0)).h_full().child(panel))
        })
        // 透明拖动层最后加入元素树，确保覆盖在面板内容之上并保持稳定命中。
        .when(!app.is_assistant_panel_collapsed, |this| {
            this.child(render_assistant_resizer(app, cx))
        });

    if app.is_assistant_panel_resizing {
        return panel.w(px(visible_width.max(0.0))).into_any_element();
    }
    panel
        .with_animation(
            (
                "assistant-panel-width",
                app.assistant_panel_animation_generation,
            ),
            Animation::new(Duration::from_millis(160)).with_easing(gpui::ease_out_quint()),
            move |this, progress| {
                let width = from_width + (to_width - from_width) * progress;
                this.w(px(width.max(0.0))).opacity(if to_width == 0.0 {
                    1.0 - progress * 0.12
                } else {
                    0.88 + progress * 0.12
                })
            },
        )
        .into_any_element()
}

/// 渲染助手面板左侧透明拖动命中区。
///
/// 命中层覆盖在 Agent 色块边缘，不占用布局宽度也不绘制分割线；主内容与侧栏仅通过
/// `content`、`side_bar` 两种背景色区分，与左侧来源树保持相同视觉逻辑。
fn render_assistant_resizer(_app: &ArgusApp, cx: &mut Context<ArgusApp>) -> impl IntoElement {
    div()
        .id("assistant-panel-resizer")
        .absolute()
        .top_0()
        .left_0()
        .w(px(6.0))
        .h_full()
        .cursor_col_resize()
        .on_hover(cx.listener(|app, hovered: &bool, _, cx| {
            if app.is_assistant_resizer_hovered != *hovered {
                app.is_assistant_resizer_hovered = *hovered;
                cx.notify();
            }
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|app, event: &MouseDownEvent, _, cx| {
                app.begin_assistant_panel_resize(event.position.x / px(1.0));
                cx.stop_propagation();
                cx.notify();
            }),
        )
}

/// 渲染可动画宽度的来源侧栏容器；内容保持原宽度，外层负责裁剪。
fn animated_source_panel(app: &ArgusApp, cx: &mut Context<ArgusApp>) -> AnyElement {
    let from_width = app.source_panel_animation_from_width;
    let to_width = app.source_panel_animation_to_width;
    let panel = div()
        .id("animated-source-panel")
        .h_full()
        .flex_none()
        .overflow_hidden()
        .child(source_panel::render(app, cx));

    if app.is_source_panel_resizing {
        return panel
            .w(px(app.current_source_panel_width().max(0.0)))
            .opacity(1.0)
            .into_any_element();
    }

    panel
        .with_animation(
            ("source-panel-width", app.source_panel_animation_generation),
            Animation::new(Duration::from_millis(170)).with_easing(gpui::ease_out_quint()),
            move |this, progress| {
                let width = from_width + (to_width - from_width) * progress;
                this.w(px(width.max(0.0))).opacity(if to_width == 0.0 {
                    1.0 - progress * 0.12
                } else {
                    0.88 + progress * 0.12
                })
            },
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证玻璃板阴影沿用 opencode v2 内容面板 `--v2-elevation-raised`（深色）的配方：
    /// 两层向下黑色投影 + 0.5px 白色外描边 + 顶部 0.5px 白色亮线，共四层且顺序固定。
    #[test]
    fn window_content_shadows_match_reference_elevation() {
        let shadows = window_content_shadows();
        assert_eq!(shadows.len(), 4, "阴影应由两层投影、外描边和顶部高光组成");

        let primary = &shadows[0];
        assert_eq!(primary.offset, point(px(0.0), px(2.0)));
        assert_eq!(primary.blur_radius, px(4.0));
        assert_eq!(primary.spread_radius, px(0.0));

        let secondary = &shadows[1];
        assert_eq!(secondary.offset, point(px(0.0), px(1.0)));
        assert_eq!(secondary.blur_radius, px(2.0));
        assert_eq!(secondary.spread_radius, px(0.0));

        let edge = &shadows[2];
        assert_eq!(edge.offset, point(px(0.0), px(0.0)));
        assert_eq!(edge.blur_radius, px(0.0));
        assert_eq!(edge.spread_radius, px(0.5), "外描边依靠 0.5px 扩散实现");

        let top_highlight = &shadows[3];
        assert_eq!(top_highlight.offset, point(px(0.0), px(-0.5)));
        assert_eq!(top_highlight.blur_radius, px(0.0));
        assert_eq!(top_highlight.spread_radius, px(0.0));

        // 投影为中性黑，描边与顶部高光为中性白，且层级越靠上不透明度越低。
        let shadow_color = hsla(0.0, 0.0, 0.0, WINDOW_CONTENT_SHADOW_OPACITY);
        assert_eq!(shadows[0].color, shadow_color);
        assert_eq!(shadows[1].color, shadow_color);
        assert_eq!(
            shadows[2].color,
            hsla(0.0, 0.0, 1.0, WINDOW_CONTENT_EDGE_OPACITY)
        );
        assert_eq!(
            shadows[3].color,
            hsla(0.0, 0.0, 1.0, WINDOW_CONTENT_TOP_HIGHLIGHT_OPACITY)
        );
        assert!(shadows[3].color.a < shadows[2].color.a);
    }
}
