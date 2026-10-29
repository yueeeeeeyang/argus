//! 文件职责：通过真实 GPUI 鼠标事件验证预览滚动条显隐、拖拽与长行范围。

use super::*;
use crate::config::ConfigManager;
use crate::config::paths::isolated_test_dir;
use gpui::{Modifiers, ScrollStrategy, TestAppContext, VisualTestContext, size};

/// 为滚动交互创建独立窗口，保留主应用以维持主题订阅。
fn scrollbar_window(
    cx: &mut TestAppContext,
    content: String,
) -> (
    Entity<ArgusApp>,
    Entity<FilePreviewWindow>,
    &mut VisualTestContext,
) {
    let app = cx.new(|_| {
        ArgusApp::new_with_config_manager(ConfigManager::new(
            isolated_test_dir("preview-scrollbars").join("settings.toml"),
        ))
    });
    let theme = cx.read(|cx| app.read(cx).theme.clone());
    let app_for_preview = app.clone();
    let (preview, cx) = cx.add_window_view(move |_, cx| {
        FilePreviewWindow::new(
            app_for_preview,
            theme,
            "scroll.log".to_string(),
            FilePreviewContent::Text {
                content,
                truncated: false,
            },
            cx,
        )
    });
    cx.simulate_resize(size(px(1000.0), px(700.0)));
    scrollbar_frame(cx);
    (app, preview, cx)
}

/// 不推进计时器，只完成一次布局，确保显隐没有依赖延迟任务。
fn scrollbar_frame(cx: &mut VisualTestContext) {
    let viewport = cx.update(|window, _| window.viewport_size());
    let width = if viewport.width == px(1000.0) {
        1001.0
    } else {
        1000.0
    };
    cx.simulate_resize(size(px(width), viewport.height));
}

/// 同时溢出两个方向，方便验证每条滚动轴的独立性。
fn overflowing_preview_text() -> String {
    (0..220)
        .map(|index| format!("{index:03} {}", "abcdefghij".repeat(45)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 首次进入正文显示两条溢出滑块，移到标题栏立即隐藏，不等待定时器。
#[gpui::test]
fn preview_scrollbars_follow_body_hover_without_hide_delay(cx: &mut TestAppContext) {
    let (_app, preview, cx) = scrollbar_window(cx, overflowing_preview_text());
    assert!(cx.debug_bounds("file-preview-scrollbar-vertical").is_none());
    assert!(
        cx.debug_bounds("file-preview-scrollbar-horizontal")
            .is_none()
    );
    let body = cx
        .debug_bounds("file-preview-scroll-area")
        .expect("正文应有视口");
    cx.simulate_mouse_move(body.center(), None, Modifiers::default());
    scrollbar_frame(cx);
    assert!(preview.read_with(cx, |view, _| view.is_body_hovered));
    assert!(cx.debug_bounds("file-preview-scrollbar-vertical").is_some());
    assert!(
        cx.debug_bounds("file-preview-scrollbar-horizontal")
            .is_some()
    );

    let previous_vertical = cx.debug_bounds("file-preview-scrollbar-vertical").unwrap();
    cx.simulate_mouse_move(point(px(500.0), px(20.0)), None, Modifiers::default());
    scrollbar_frame(cx);
    assert!(!preview.read_with(cx, |view, _| view.is_body_hovered));
    // GPUI 的调试位置表不会逐帧清理；用当前元素树和真实命中验证隐藏，不能依赖旧 selector 消失。
    preview.update(cx, |view, cx| {
        assert!(view.render_preview_scrollbars(cx).is_empty())
    });
    cx.simulate_mouse_down(
        previous_vertical.center(),
        MouseButton::Left,
        Modifiers::default(),
    );
    assert!(
        preview.read_with(cx, |view, _| view.scrollbar_drag.is_none()),
        "隐藏滑块不能拦截原位置的正文点击"
    );
    cx.simulate_mouse_up(
        previous_vertical.center(),
        MouseButton::Left,
        Modifiers::default(),
    );
}

/// 鼠标在正文内并不代表有滚动范围；单行短文不应出现空滚动条。
#[gpui::test]
fn preview_scrollbars_do_not_show_for_short_content(cx: &mut TestAppContext) {
    let (_app, preview, cx) = scrollbar_window(cx, "短文本".to_string());
    let body = cx.debug_bounds("file-preview-scroll-area").unwrap();
    cx.simulate_mouse_move(body.center(), None, Modifiers::default());
    scrollbar_frame(cx);
    assert!(preview.read_with(cx, |view, _| view.is_body_hovered));
    assert!(cx.debug_bounds("file-preview-scrollbar-vertical").is_none());
    assert!(
        cx.debug_bounds("file-preview-scrollbar-horizontal")
            .is_none()
    );
}

/// 横纵滑块只改变各自滚动轴，不能接管正文选区；拖出视口仍能正确释放。
#[gpui::test]
fn preview_scrollbar_drag_preserves_other_axis_and_selection(cx: &mut TestAppContext) {
    let (_app, preview, cx) = scrollbar_window(cx, overflowing_preview_text());
    let selection = PreviewTextSelection {
        anchor: PreviewTextPosition { line: 0, column: 1 },
        focus: PreviewTextPosition { line: 0, column: 5 },
    };
    preview.update(cx, |view, cx| {
        view.scroll
            .0
            .as_ref()
            .borrow()
            .base_handle
            .set_offset(point(px(-100.0), px(-200.0)));
        view.preview_selection = Some(selection.clone());
        cx.notify();
    });
    scrollbar_frame(cx);
    let body = cx.debug_bounds("file-preview-scroll-area").unwrap();
    cx.simulate_mouse_move(body.center(), None, Modifiers::default());
    scrollbar_frame(cx);
    let before = preview.read_with(cx, |view, _| {
        view.scroll.0.as_ref().borrow().base_handle.offset()
    });
    let vertical = cx
        .debug_bounds("file-preview-scrollbar-vertical")
        .expect("纵向滑块应可见");
    let vertical_target = vertical.center() + point(px(0.0), px(70.0));
    cx.simulate_mouse_down(vertical.center(), MouseButton::Left, Modifiers::default());
    assert!(preview.read_with(cx, |view, _| view.scrollbar_drag.is_some()));
    cx.simulate_mouse_move(
        vertical_target,
        Some(MouseButton::Left),
        Modifiers::default(),
    );
    cx.simulate_mouse_up(vertical_target, MouseButton::Left, Modifiers::default());
    scrollbar_frame(cx);
    let after_vertical = preview.read_with(cx, |view, _| {
        assert!(view.scrollbar_drag.is_none());
        assert_eq!(view.preview_selection.as_ref(), Some(&selection));
        view.scroll.0.as_ref().borrow().base_handle.offset()
    });
    assert_eq!(after_vertical.x, before.x);
    assert!(after_vertical.y < before.y);

    cx.simulate_mouse_move(body.center(), None, Modifiers::default());
    scrollbar_frame(cx);
    let horizontal = cx
        .debug_bounds("file-preview-scrollbar-horizontal")
        .expect("横向滑块应可见");
    let horizontal_target = horizontal.center() + point(px(70.0), px(0.0));
    cx.simulate_mouse_down(horizontal.center(), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(
        horizontal_target,
        Some(MouseButton::Left),
        Modifiers::default(),
    );
    cx.simulate_mouse_up(horizontal_target, MouseButton::Left, Modifiers::default());
    scrollbar_frame(cx);
    let after_horizontal = preview.read_with(cx, |view, _| {
        assert!(view.scrollbar_drag.is_none());
        assert_eq!(view.preview_selection.as_ref(), Some(&selection));
        view.scroll.0.as_ref().borrow().base_handle.offset()
    });
    assert_eq!(after_horizontal.y, after_vertical.y);
    assert!(after_horizontal.x < after_vertical.x);

    cx.simulate_mouse_move(body.center(), None, Modifiers::default());
    scrollbar_frame(cx);
    let vertical = cx.debug_bounds("file-preview-scrollbar-vertical").unwrap();
    cx.simulate_mouse_down(vertical.center(), MouseButton::Left, Modifiers::default());
    let outside = point(px(500.0), px(20.0));
    cx.simulate_mouse_move(outside, Some(MouseButton::Left), Modifiers::default());
    scrollbar_frame(cx);
    assert!(!preview.read_with(cx, |view, _| view.is_body_hovered));
    assert!(preview.read_with(cx, |view, _| view.scrollbar_drag.is_some()));
    preview.update(cx, |view, cx| {
        assert!(view.render_preview_scrollbars(cx).is_empty())
    });
    cx.simulate_mouse_up(outside, MouseButton::Left, Modifiers::default());
    assert!(preview.read_with(cx, |view, _| view.scrollbar_drag.is_none()));
    assert_eq!(
        preview.read_with(cx, |view, _| view.preview_selection.clone()),
        Some(selection)
    );
}

/// 各独立预览窗口保存自己的悬停状态，不能因另一个窗口进入或离开而显示滑块。
#[gpui::test]
fn preview_scrollbar_hover_state_is_isolated_between_windows(cx: &mut TestAppContext) {
    let (_first_app, first, first_cx) = scrollbar_window(cx, overflowing_preview_text());
    let body = first_cx.debug_bounds("file-preview-scroll-area").unwrap();
    first_cx.simulate_mouse_move(body.center(), None, Modifiers::default());
    scrollbar_frame(first_cx);
    assert!(first.read_with(first_cx, |view, _| view.is_body_hovered));

    let (_second_app, second, second_cx) = scrollbar_window(cx, overflowing_preview_text());
    assert!(!second.read_with(second_cx, |view, _| view.is_body_hovered));
    assert!(
        second_cx
            .debug_bounds("file-preview-scrollbar-vertical")
            .is_none()
    );
    assert!(
        second_cx
            .debug_bounds("file-preview-scrollbar-horizontal")
            .is_none()
    );
    let second_body = second_cx.debug_bounds("file-preview-scroll-area").unwrap();
    second_cx.simulate_mouse_move(second_body.center(), None, Modifiers::default());
    scrollbar_frame(second_cx);
    second_cx.simulate_mouse_move(point(px(500.0), px(20.0)), None, Modifiers::default());
    scrollbar_frame(second_cx);
    assert!(!second.read_with(second_cx, |view, _| view.is_body_hovered));
    assert!(
        first.read_with(second_cx, |view, _| view.is_body_hovered),
        "第二窗口事件不应修改第一窗口状态"
    );
}

/// 原生窗口离开事件可能仍携带正文内坐标；隐藏不能依赖坐标越界或窗口失焦。
#[gpui::test]
fn preview_scrollbars_hide_on_window_exit_and_restore_at_same_position(cx: &mut TestAppContext) {
    let (_app, preview, cx) = scrollbar_window(cx, overflowing_preview_text());
    let body = cx.debug_bounds("file-preview-scroll-area").unwrap();
    let body_position = body.center();
    for exit_position in [body_position, point(px(-20.0), px(-20.0))] {
        cx.simulate_mouse_move(body_position, None, Modifiers::default());
        scrollbar_frame(cx);
        assert!(preview.read_with(cx, |view, _| view.is_body_hovered));
        preview.update(cx, |view, cx| {
            assert_eq!(view.render_preview_scrollbars(cx).len(), 2);
        });
        let was_active = cx.update(|window, _| window.is_window_active());

        cx.simulate_event(gpui::MouseExitEvent {
            position: exit_position,
            ..Default::default()
        });
        assert_eq!(cx.update(|window, _| window.is_window_active()), was_active);
        assert!(!preview.read_with(cx, |view, _| view.is_body_hovered));
        preview.update(cx, |view, cx| {
            assert!(view.render_preview_scrollbars(cx).is_empty());
        });
        // 重绘不能根据离开前缓存的正文命中状态把已隐藏的滑块重新显示。
        scrollbar_frame(cx);
        assert!(!preview.read_with(cx, |view, _| view.is_body_hovered));

        cx.simulate_mouse_move(body_position, None, Modifiers::default());
        scrollbar_frame(cx);
        assert!(preview.read_with(cx, |view, _| view.is_body_hovered));
        preview.update(cx, |view, cx| {
            assert_eq!(view.render_preview_scrollbars(cx).len(), 2);
        });
    }
}

/// 鼠标移入同应用的另一窗口后，旧预览必须保持隐藏，直到真正重新进入自己的正文。
#[gpui::test]
fn preview_scrollbars_stay_hidden_while_pointer_moves_in_another_window(cx: &mut TestAppContext) {
    let (_first_app, first, first_cx) = scrollbar_window(cx, overflowing_preview_text());
    let mut first_cx = first_cx.clone();
    let first_position = first_cx
        .debug_bounds("file-preview-scroll-area")
        .unwrap()
        .center();
    first_cx.simulate_mouse_move(first_position, None, Modifiers::default());
    scrollbar_frame(&mut first_cx);
    assert!(first.read_with(&first_cx, |view, _| view.is_body_hovered));
    first_cx.simulate_event(gpui::MouseExitEvent {
        position: first_position,
        ..Default::default()
    });

    let (_second_app, second, second_cx) = scrollbar_window(cx, overflowing_preview_text());
    let second_position = second_cx
        .debug_bounds("file-preview-scroll-area")
        .unwrap()
        .center();
    second_cx.simulate_mouse_move(second_position, None, Modifiers::default());
    scrollbar_frame(second_cx);
    assert!(second.read_with(second_cx, |view, _| view.is_body_hovered));
    assert!(!first.read_with(second_cx, |view, _| view.is_body_hovered));
    first.update(second_cx, |view, cx| {
        assert!(view.render_preview_scrollbars(cx).is_empty());
    });
    second_cx.simulate_mouse_move(point(px(500.0), px(20.0)), None, Modifiers::default());
    scrollbar_frame(second_cx);
    assert!(!first.read_with(second_cx, |view, _| view.is_body_hovered));

    first_cx.simulate_mouse_move(first_position, None, Modifiers::default());
    scrollbar_frame(&mut first_cx);
    assert!(first.read_with(&first_cx, |view, _| view.is_body_hovered));
    first_cx.simulate_mouse_move(point(px(500.0), px(20.0)), None, Modifiers::default());
    scrollbar_frame(&mut first_cx);
    assert!(!first.read_with(&first_cx, |view, _| view.is_body_hovered));
}

/// 在窗口外松开滑块可能收不到 MouseUp；返回时必须结束旧拖拽，且不能改变正文选区或滚动位置。
#[gpui::test]
fn preview_scrollbar_window_exit_does_not_leave_a_stuck_drag(cx: &mut TestAppContext) {
    let (_app, preview, cx) = scrollbar_window(cx, overflowing_preview_text());
    let selection = PreviewTextSelection {
        anchor: PreviewTextPosition { line: 0, column: 1 },
        focus: PreviewTextPosition { line: 0, column: 5 },
    };
    preview.update(cx, |view, _| {
        view.preview_selection = Some(selection.clone())
    });
    let body_position = cx
        .debug_bounds("file-preview-scroll-area")
        .unwrap()
        .center();
    cx.simulate_mouse_move(body_position, None, Modifiers::default());
    scrollbar_frame(cx);
    let thumb = cx.debug_bounds("file-preview-scrollbar-vertical").unwrap();
    cx.simulate_mouse_down(thumb.center(), MouseButton::Left, Modifiers::default());
    assert!(preview.read_with(cx, |view, _| view.scrollbar_drag.is_some()));
    let offset = preview.read_with(cx, |view, _| {
        view.scroll.0.as_ref().borrow().base_handle.offset()
    });

    cx.simulate_event(gpui::MouseExitEvent {
        position: thumb.center(),
        pressed_button: Some(MouseButton::Left),
        ..Default::default()
    });
    assert!(!preview.read_with(cx, |view, _| view.is_body_hovered));
    preview.update(cx, |view, cx| {
        assert!(view.render_preview_scrollbars(cx).is_empty())
    });
    cx.simulate_mouse_move(body_position, None, Modifiers::default());
    scrollbar_frame(cx);
    preview.read_with(cx, |view, _| {
        assert!(view.is_body_hovered);
        assert!(view.scrollbar_drag.is_none());
        assert_eq!(view.preview_selection.as_ref(), Some(&selection));
        assert_eq!(view.scroll.0.as_ref().borrow().base_handle.offset(), offset);
    });
}

/// 字节最长并非像素最宽：较短字节的 ASCII 行首次可见后必须补足横向滚动范围。
#[gpui::test]
fn preview_scrollbar_width_expands_for_first_visible_wider_ascii_line(cx: &mut TestAppContext) {
    let chinese = "中".repeat(400);
    let ascii = "W".repeat(1100);
    assert!(chinese.len() > ascii.len());
    let mut lines = vec![chinese];
    lines.extend((0..150).map(|index| format!("row {index}")));
    let target_index = lines.len();
    lines.push(ascii.clone());
    let (_app, preview, cx) = scrollbar_window(cx, lines.join("\n"));
    let initial_width = preview.read_with(cx, |view, _| {
        assert_eq!(view.longest_line_index, 0);
        assert!(!view.measured_content_lines[target_index]);
        view.content_min_width
    });
    let expected_width = cx.update(|window, cx| {
        let view = preview.read(cx);
        let mut style = window.text_style();
        style.font_family = ARGUS_LOG_FONT_FAMILY.into();
        let shaped = window.text_system().shape_line(
            SharedString::from(ascii.clone()),
            px(FILE_PREVIEW_FONT_SIZE),
            &[TextRun {
                len: ascii.len(),
                font: style.font(),
                color: style.color,
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        );
        shaped.width
            + px(
                log_viewer_line_number_width(view.text_lines().unwrap().len())
                    + FILE_PREVIEW_TEXT_LEFT_PADDING * 2.0,
            )
    });
    assert!(
        expected_width > initial_width,
        "测试必须覆盖字节短但像素更宽的文本"
    );
    preview.update(cx, |view, cx| {
        view.scroll
            .scroll_to_item(target_index, ScrollStrategy::Center);
        cx.notify();
    });
    scrollbar_frame(cx);
    scrollbar_frame(cx);
    preview.read_with(cx, |view, _| {
        assert!(view.measured_content_lines[target_index]);
        assert!(view.content_min_width >= expected_width);
        let actual_width = view.scroll.0.as_ref().borrow().last_item_size.unwrap().contents.width;
        // GPUI 将布局尺寸对齐到像素，字形排版仍保留小数；容差仅覆盖这一步取整。
        assert!(actual_width + px(1.0) >= expected_width, "内容布局宽度应覆盖实测宽度：minimum={:?}, actual={actual_width:?}, expected={expected_width:?}", view.content_min_width);
    });
    let body = cx.debug_bounds("file-preview-scroll-area").unwrap();
    cx.simulate_mouse_move(body.center(), None, Modifiers::default());
    scrollbar_frame(cx);
    let thumb = cx
        .debug_bounds("file-preview-scrollbar-horizontal")
        .unwrap();
    cx.simulate_mouse_down(thumb.center(), MouseButton::Left, Modifiers::default());
    let right_edge = point(body.right() - px(1.0), thumb.center().y);
    cx.simulate_mouse_move(right_edge, Some(MouseButton::Left), Modifiers::default());
    cx.simulate_mouse_up(right_edge, MouseButton::Left, Modifiers::default());
    scrollbar_frame(cx);
    preview.read_with(cx, |view, _| {
        let state = view.scroll.0.as_ref().borrow();
        let visible_right = -state.base_handle.offset().x + state.base_handle.bounds().size.width;
        assert!(
            visible_right >= expected_width - px(1.0),
            "横向拖到底必须覆盖实际行尾"
        );
    });
}

/// 覆盖滑块的命中区域必须继续透传滚轮，搜索输入焦点与正文选区保持不变。
#[gpui::test]
fn preview_scrollbar_wheel_keeps_search_focus_and_body_selection(cx: &mut TestAppContext) {
    let (_app, preview, cx) = scrollbar_window(cx, overflowing_preview_text());
    let body = cx.debug_bounds("file-preview-scroll-area").unwrap();
    cx.simulate_mouse_move(body.center(), None, Modifiers::default());
    cx.simulate_keystrokes("ctrl-f");
    scrollbar_frame(cx);
    let selection = PreviewTextSelection {
        anchor: PreviewTextPosition { line: 0, column: 0 },
        focus: PreviewTextPosition { line: 0, column: 3 },
    };
    preview.update(cx, |view, _| {
        view.preview_selection = Some(selection.clone())
    });
    let vertical = cx.debug_bounds("file-preview-scrollbar-vertical").unwrap();
    cx.simulate_event(gpui::ScrollWheelEvent {
        position: vertical.center(),
        delta: gpui::ScrollDelta::Pixels(point(px(0.0), px(-80.0))),
        ..Default::default()
    });
    scrollbar_frame(cx);
    let after_vertical = preview.read_with(cx, |view, _| {
        view.scroll.0.as_ref().borrow().base_handle.offset()
    });
    assert!(after_vertical.y < px(0.0));
    assert_eq!(after_vertical.x, px(0.0));
    let horizontal = cx
        .debug_bounds("file-preview-scrollbar-horizontal")
        .unwrap();
    cx.simulate_event(gpui::ScrollWheelEvent {
        position: horizontal.center(),
        delta: gpui::ScrollDelta::Pixels(point(px(-80.0), px(0.0))),
        ..Default::default()
    });
    scrollbar_frame(cx);
    cx.update(|window, cx| {
        let view = preview.read(cx);
        let offset = view.scroll.0.as_ref().borrow().base_handle.offset();
        assert!(offset.x < px(0.0));
        assert_eq!(offset.y, after_vertical.y);
        assert_eq!(view.preview_selection.as_ref(), Some(&selection));
        assert!(view.search_focus.is_focused(window));
        assert!(view.scrollbar_drag.is_none());
    });
}
