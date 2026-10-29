//! 文件职责：通过独立 GPUI 窗口验证预览搜索的焦点、原生输入、快捷键和命中定位。

use super::*;
use crate::config::ConfigManager;
use crate::config::paths::isolated_test_dir;
use crate::infra::text_selection::NativeTextEdit;
use gpui::{Modifiers, TestAppContext, VisualTestContext, size};
use std::sync::atomic::Ordering;
use std::time::Duration;

/// 保留主应用实体以维持主题订阅，窗口初始焦点完全由生产逻辑设置。
fn preview_window(
    cx: &mut TestAppContext,
    content: FilePreviewContent,
) -> (
    Entity<ArgusApp>,
    Entity<FilePreviewWindow>,
    &mut VisualTestContext,
) {
    let app = cx.new(|_| {
        ArgusApp::new_with_config_manager(ConfigManager::new(
            isolated_test_dir("file-preview-search").join("settings.toml"),
        ))
    });
    let theme = cx.read(|cx| app.read(cx).theme.clone());
    let app_for_preview = app.clone();
    let (preview, cx) = cx.add_window_view(move |_, cx| {
        FilePreviewWindow::new(
            app_for_preview,
            theme,
            "preview.log".to_string(),
            content,
            cx,
        )
    });
    cx.simulate_resize(size(px(1000.0), px(700.0)));
    (app, preview, cx)
}

/// 强制新帧安装输入处理器并应用滚动定位，避免依赖测试窗口的绘制时机。
fn draw_preview_frame(cx: &mut VisualTestContext) {
    let current = cx.update(|window, _| window.viewport_size());
    let width = if current.width == px(1000.0) {
        1001.0
    } else {
        1000.0
    };
    cx.simulate_resize(size(px(width), current.height));
}

/// 推进可控防抖计时器，并等待后台搜索及结果发布完成。
fn finish_preview_search(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    draw_preview_frame(cx);
}

/// 按正文实际字体测量命中，防止滚动偏移虽为负但目标仍被视口裁掉。
fn assert_active_preview_match_is_visible(
    preview: &Entity<FilePreviewWindow>,
    cx: &mut VisualTestContext,
) {
    cx.update(|window, cx| {
        let view = preview.read(cx);
        let hit = &view.search.results.matches[view.search.active_index.expect("应有当前命中")];
        let lines = view.text_lines().expect("应有文本正文");
        let line = &lines[hit.line];
        let mut text_style = window.text_style();
        text_style.font_family = ARGUS_LOG_FONT_FAMILY.into();
        text_style.font_size = px(FILE_PREVIEW_FONT_SIZE).into();
        let shaped = window.text_system().shape_line(
            SharedString::from(line.clone()),
            px(FILE_PREVIEW_FONT_SIZE),
            &[TextRun {
                len: line.len(),
                font: text_style.font(),
                color: text_style.color,
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        );
        let scroll = view.scroll.0.as_ref().borrow();
        let bounds = scroll.base_handle.bounds();
        let offset = scroll.base_handle.offset();
        let text_left = bounds.left()
            + offset.x
            + px(log_viewer_line_number_width(lines.len()) + FILE_PREVIEW_TEXT_LEFT_PADDING);
        let match_left = text_left + shaped.x_for_index(hit.range.start);
        let match_right = text_left + shaped.x_for_index(hit.range.end);
        let row_top = bounds.top() + offset.y + px(hit.line as f32 * FILE_PREVIEW_ROW_HEIGHT);
        assert!(
            match_left >= bounds.left() - px(1.0) && match_right <= bounds.right() + px(1.0),
            "命中字形必须完整进入横向视口：命中 {match_left:?}..{match_right:?}，视口 {bounds:?}，偏移 {offset:?}"
        );
        assert!(
            row_top >= bounds.top() - px(1.0)
                && row_top + px(FILE_PREVIEW_ROW_HEIGHT) <= bounds.bottom() + px(1.0),
            "命中行必须完整进入纵向视口：行顶部 {row_top:?}，视口 {bounds:?}，偏移 {offset:?}"
        );
    });
}

/// 首次打开无需点击正文；点击搜索框后真实焦点不会被根点击处理器抢走。
#[gpui::test]
fn preview_search_shortcut_and_pointer_focus_preserve_native_unicode_input(
    cx: &mut TestAppContext,
) {
    let (_app, preview, cx) = preview_window(
        cx,
        FilePreviewContent::Text {
            content: "中文 first\nsecond 中文".to_string(),
            truncated: false,
        },
    );
    cx.simulate_keystrokes("ctrl-f");
    draw_preview_frame(cx);
    assert!(cx.debug_bounds("file-preview-search-bar").is_some());
    let bounds = cx
        .debug_bounds("file-preview-search-input")
        .expect("应显示搜索输入框");
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.update(|window, cx| {
        let view = preview.read(cx);
        assert!(view.search.is_open);
        assert!(view.search.input.is_focused);
        assert!(view.search_focus.is_focused(window));
        assert!(!view.root_focus.is_focused(window));
    });
    cx.simulate_input("中文");
    finish_preview_search(cx);
    preview.update(cx, |view, _| {
        assert_eq!(
            view.search.input.value, "中文",
            "普通字符只能由原生输入桥写入一次"
        );
        assert_eq!(view.search.results.matches.len(), 2);
        assert!(!view.search.is_searching);
    });

    let close = cx
        .debug_bounds("file-preview-search-close")
        .expect("应显示关闭按钮");
    cx.simulate_click(close.center(), Modifiers::default());
    cx.update(|window, cx| {
        let view = preview.read(cx);
        assert!(!view.search.is_open);
        assert!(view.root_focus.is_focused(window));
    });
    #[cfg(target_os = "macos")]
    {
        cx.simulate_keystrokes("cmd-f");
        draw_preview_frame(cx);
        assert!(
            preview.read_with(cx, |view, _| view.search.is_open),
            "macOS 同时支持 Cmd+F"
        );
    }
}

/// 搜索输入的全选和剪贴板不能落到正文；关闭搜索后正文快捷键恢复。
#[gpui::test]
fn preview_search_clipboard_shortcuts_do_not_modify_body_selection(cx: &mut TestAppContext) {
    let document = "正文 needle\nneedle 末尾";
    let (_app, preview, cx) = preview_window(
        cx,
        FilePreviewContent::Text {
            content: document.to_string(),
            truncated: false,
        },
    );
    cx.simulate_keystrokes("ctrl-f");
    draw_preview_frame(cx);
    cx.simulate_input("needle");
    finish_preview_search(cx);
    let body_selection = PreviewTextSelection {
        anchor: PreviewTextPosition { line: 0, column: 0 },
        focus: PreviewTextPosition { line: 0, column: 1 },
    };
    preview.update(cx, |view, _| {
        view.preview_selection = Some(body_selection.clone())
    });

    cx.simulate_keystrokes("ctrl-a ctrl-c");
    assert_eq!(
        cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()))
            .as_deref(),
        Some("needle")
    );
    assert_eq!(
        preview.read_with(cx, |view, _| view.preview_selection.clone()),
        Some(body_selection.clone())
    );
    cx.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("替换\n文字".to_string())));
    cx.simulate_keystrokes("ctrl-v");
    finish_preview_search(cx);
    preview.update(cx, |view, _| {
        assert_eq!(view.search.input.value, "替换 文字");
        assert_eq!(view.preview_selection.as_ref(), Some(&body_selection));
        assert!(view.search.results.matches.is_empty());
        assert!(!view.search.is_searching);
    });

    cx.simulate_keystrokes("escape ctrl-a ctrl-c");
    assert_eq!(
        cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()))
            .as_deref(),
        Some(document)
    );
    assert!(!preview.read_with(cx, |view, _| view.search.is_open));
}

/// 回车按文档顺序循环，大小写按钮改变结果且不会让搜索框失焦。
#[gpui::test]
fn preview_search_navigation_wraps_and_case_button_keeps_input_focus(cx: &mut TestAppContext) {
    let (_app, preview, cx) = preview_window(
        cx,
        FilePreviewContent::Text {
            content: "needle\nNEEDLE\nneedle".to_string(),
            truncated: false,
        },
    );
    cx.simulate_keystrokes("ctrl-f");
    draw_preview_frame(cx);
    cx.simulate_input("needle");
    finish_preview_search(cx);
    assert_eq!(
        preview.read_with(cx, |view, _| view.search.results.matches.len()),
        3
    );
    assert_eq!(
        preview.read_with(cx, |view, _| view.search.active_index),
        Some(0)
    );
    cx.simulate_keystrokes("enter");
    assert_eq!(
        preview.read_with(cx, |view, _| view.search.active_index),
        Some(1)
    );
    cx.simulate_keystrokes("shift-enter shift-enter");
    assert_eq!(
        preview.read_with(cx, |view, _| view.search.active_index),
        Some(2)
    );
    cx.simulate_keystrokes("enter");
    assert_eq!(
        preview.read_with(cx, |view, _| view.search.active_index),
        Some(0)
    );

    let button = cx
        .debug_bounds("file-preview-search-case")
        .expect("应显示大小写开关");
    cx.simulate_click(button.center(), Modifiers::default());
    finish_preview_search(cx);
    cx.update(|window, cx| {
        let view = preview.read(cx);
        assert!(view.search.is_case_sensitive);
        assert_eq!(view.search.results.matches.len(), 2);
        assert!(view.search_focus.is_focused(window));
    });
}

/// 候选文本只更新输入状态，提交后才发布搜索结果；关闭时取消未完成的查询。
#[gpui::test]
fn preview_search_native_composition_waits_for_commit_and_close_cancels_pending(
    cx: &mut TestAppContext,
) {
    let (_app, preview, cx) = preview_window(
        cx,
        FilePreviewContent::Text {
            content: "中文\n中文".to_string(),
            truncated: false,
        },
    );
    cx.simulate_keystrokes("ctrl-f");
    preview.update(cx, |view, cx| {
        view.apply_search_native_edit(
            NativeTextEdit {
                replacement_range: 0..0,
                text: "中".to_string(),
                selected_range: 1..1,
                marked_range: Some(0..1),
            },
            cx,
        );
        assert_eq!(view.search.input.marked_range, Some(0..1));
        assert!(!view.search.is_searching);
        assert!(view.search.results.matches.is_empty());
    });
    cx.simulate_keystrokes("escape");
    assert!(
        preview.read_with(cx, |view, _| view.search.is_open),
        "输入法候选态 Escape 不应关闭整个查找条"
    );
    preview.update(cx, |view, cx| {
        view.apply_search_native_edit(
            NativeTextEdit {
                replacement_range: 0..1,
                text: "中文".to_string(),
                selected_range: 2..2,
                marked_range: None,
            },
            cx,
        )
    });
    finish_preview_search(cx);
    assert_eq!(
        preview.read_with(cx, |view, _| view.search.results.matches.len()),
        2
    );
    cx.update(|window, cx| {
        preview.update(cx, |view, cx| {
            view.apply_search_native_edit(
                NativeTextEdit {
                    replacement_range: 0..2,
                    text: "next".to_string(),
                    selected_range: 4..4,
                    marked_range: None,
                },
                cx,
            );
            let cancel = view
                .search
                .cancel_token
                .as_ref()
                .expect("应已有待执行查询")
                .clone();
            view.close_search(window, cx);
            assert!(cancel.load(Ordering::Relaxed));
        })
    });
    finish_preview_search(cx);
    preview.read_with(cx, |view, _| {
        assert!(!view.search.is_open);
        assert!(!view.search.is_searching);
        assert!(view.search.results.matches.is_empty());
    });
}

/// 后续命中位于视口外的长行时，导航同时移动纵向与横向滚动位置。
#[gpui::test]
fn preview_search_navigation_reveals_match_in_offscreen_long_line(cx: &mut TestAppContext) {
    let mut lines = vec!["target".to_string()];
    lines.extend((0..100).map(|index| format!("日志第 {index} 行")));
    lines.push(format!("{}target", "长行文本".repeat(150)));
    let (_app, preview, cx) = preview_window(
        cx,
        FilePreviewContent::Text {
            content: lines.join("\n"),
            truncated: false,
        },
    );
    cx.simulate_keystrokes("ctrl-f");
    draw_preview_frame(cx);
    cx.simulate_input("target");
    finish_preview_search(cx);
    assert_eq!(
        preview.read_with(cx, |view, _| view.search.results.matches.len()),
        2
    );
    cx.simulate_keystrokes("enter");
    draw_preview_frame(cx);
    draw_preview_frame(cx);
    preview.update(cx, |view, _| {
        assert_eq!(view.search.active_index, Some(1));
        assert_eq!(view.search.results.matches[1].line, lines.len() - 1);
        let scroll = view.scroll.0.as_ref().borrow();
        let offset = scroll.base_handle.offset();
        assert!(offset.y < px(0.0), "命中所在行应滚入纵向视口：{offset:?}");
        assert!(offset.x < px(0.0), "长行末尾命中应滚入横向视口：{offset:?}");
    });
    assert_active_preview_match_is_visible(&preview, cx);

    cx.simulate_keystrokes("shift-enter");
    draw_preview_frame(cx);
    draw_preview_frame(cx);
    preview.read_with(cx, |view, _| {
        assert_eq!(view.search.active_index, Some(0));
        assert_eq!(
            view.scroll.0.as_ref().borrow().base_handle.offset().x,
            px(0.0),
            "返回能完整显示的短行后应恢复左端，保留完整行号和正文"
        );
    });
    assert_active_preview_match_is_visible(&preview, cx);
}

/// 二进制和读取错误窗口没有可搜索正文，不应打开可输入的查找条。
#[gpui::test]
fn preview_search_ignores_shortcut_without_text_content(cx: &mut TestAppContext) {
    for content in [
        FilePreviewContent::Binary,
        FilePreviewContent::Error("读取失败".to_string()),
    ] {
        let (_app, preview, cx) = preview_window(cx, content);
        cx.simulate_keystrokes("ctrl-f");
        draw_preview_frame(cx);
        assert!(!preview.read_with(cx, |view, _| view.search.is_open));
        assert!(cx.debug_bounds("file-preview-search-bar").is_none());
    }
}

/// 防抖期间连续替换查询必须取消旧快照，旧任务不能覆盖最后一次输入的结果。
#[gpui::test]
fn preview_search_rapid_input_only_publishes_latest_query(cx: &mut TestAppContext) {
    let (_app, preview, cx) = preview_window(
        cx,
        FilePreviewContent::Text {
            content: "first first\nlast".to_string(),
            truncated: false,
        },
    );
    cx.simulate_keystrokes("ctrl-f");
    draw_preview_frame(cx);
    cx.simulate_input("first");
    let previous_cancel = preview.read_with(cx, |view, _| {
        assert!(view.search.is_searching);
        view.search.cancel_token.as_ref().unwrap().clone()
    });
    cx.simulate_keystrokes("ctrl-a");
    cx.simulate_input("last");
    assert!(previous_cancel.load(Ordering::Relaxed));
    finish_preview_search(cx);
    preview.read_with(cx, |view, _| {
        assert_eq!(view.search.input.value, "last");
        assert!(!view.search.is_searching);
        assert_eq!(view.search.results.matches.len(), 1);
        assert_eq!(view.search.results.matches[0].line, 1);
        assert_eq!(view.search.results.matches[0].range, 0..4);
    });
    finish_preview_search(cx);
    assert_eq!(
        preview.read_with(cx, |view, _| view.search.results.matches.len()),
        1
    );
}

/// 空文本仍可打开查找条，非空查询正常结束并显示零结果。
#[gpui::test]
fn preview_search_empty_document_finishes_with_zero_matches(cx: &mut TestAppContext) {
    let (_app, preview, cx) = preview_window(
        cx,
        FilePreviewContent::Text {
            content: String::new(),
            truncated: false,
        },
    );
    cx.simulate_keystrokes("ctrl-f");
    draw_preview_frame(cx);
    assert!(cx.debug_bounds("file-preview-search-input").is_some());
    cx.simulate_input("missing");
    finish_preview_search(cx);
    preview.read_with(cx, |view, _| {
        assert!(view.search.is_open);
        assert_eq!(view.search.input.value, "missing");
        assert!(view.search.results.matches.is_empty());
        assert!(view.search.active_index.is_none());
        assert!(!view.search.is_searching);
        assert!(view.search.error.is_none());
    });
}

/// 真实点击正文后输入不再修改查询，再次打开查找会恢复输入焦点并全选原查询。
#[gpui::test]
fn preview_search_body_click_releases_input_focus_and_shortcut_restores_it(
    cx: &mut TestAppContext,
) {
    let (_app, preview, cx) = preview_window(
        cx,
        FilePreviewContent::Text {
            content: "needle has enough text for pointer selection\nlast tail".to_string(),
            truncated: false,
        },
    );
    cx.simulate_keystrokes("ctrl-f");
    draw_preview_frame(cx);
    cx.simulate_input("needle");
    finish_preview_search(cx);
    let body_point = preview.read_with(cx, |view, _| {
        let bounds = view.scroll.0.as_ref().borrow().base_handle.bounds();
        point(
            bounds.left() + px(100.0),
            bounds.top() + px(FILE_PREVIEW_ROW_HEIGHT / 2.0),
        )
    });
    cx.simulate_click(body_point, Modifiers::default());
    draw_preview_frame(cx);
    cx.update(|window, cx| {
        let view = preview.read(cx);
        assert!(view.root_focus.is_focused(window));
        assert!(!view.search_focus.is_focused(window));
    });
    cx.simulate_input("ignored");
    assert_eq!(
        preview.read_with(cx, |view, _| view.search.input.value.clone()),
        "needle"
    );
    cx.simulate_keystrokes("ctrl-f");
    draw_preview_frame(cx);
    cx.update(|window, cx| assert!(preview.read(cx).search_focus.is_focused(window)));
    cx.simulate_input("tail");
    finish_preview_search(cx);
    preview.read_with(cx, |view, _| {
        assert_eq!(view.search.input.value, "tail");
        assert_eq!(view.search.results.matches.len(), 1);
        assert_eq!(view.search.results.matches[0].line, 1);
    });
}
