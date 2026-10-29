//! 文件职责：验证预览按文件类型绑定文档高亮，跳行和搜索不会丢失多行注释状态。

use super::*;
use crate::config::ConfigManager;
use crate::config::paths::isolated_test_dir;
use crate::highlight::HighlightTokenKind;
use gpui::{ScrollStrategy, TestAppContext, size};
use std::time::Duration;

/// 实际预览直接跳入 Javadoc 中段，既不能按代码着色，也不能让后续代码继续保持注释色。
#[gpui::test]
fn java_preview_keeps_document_comments_when_scrolled_and_searched(cx: &mut TestAppContext) {
    let app = cx.new(|_| {
        ArgusApp::new_with_config_manager(ConfigManager::new(
            isolated_test_dir("preview-document-highlight").join("settings.toml"),
        ))
    });
    let theme = cx.read(|cx| app.read(cx).theme.clone());
    let mut lines = vec!["/**".to_string()];
    lines.extend((0..100).map(|_| " * 中文说明 public SELECT 123".to_string()));
    lines.push(" */ public class Example {}".to_string());
    let content = lines.join("\n");
    let app_for_preview = app.clone();
    let (preview, cx) = cx.add_window_view(move |_, cx| {
        FilePreviewWindow::new(
            app_for_preview,
            theme,
            "Example.java".to_string(),
            FilePreviewContent::Text {
                content,
                truncated: false,
            },
            cx,
        )
    });
    cx.simulate_resize(size(px(900.0), px(600.0)));
    preview.update(cx, |view, cx| {
        view.scroll.scroll_to_item(60, ScrollStrategy::Center);
        cx.notify();
    });
    cx.simulate_resize(size(px(901.0), px(600.0)));
    preview.read_with(cx, |view, _| {
        assert_eq!(view.language, HighlightLanguage::Java);
        let FilePreviewBody::Text { highlighter, .. } = &view.body else {
            panic!("Java 文件应创建文本预览");
        };
        // 先读闭合行，再读中段，刻意与文件顺序不同。
        let closing = highlighter.highlight_line(101);
        assert!(
            closing
                .iter()
                .any(|span| span.kind == HighlightTokenKind::Keyword
                    && &lines[101][span.range.clone()] == "public")
        );
        assert_eq!(
            highlighter.highlight_line(60),
            vec![HighlightSpan {
                range: 0..lines[60].len(),
                kind: HighlightTokenKind::Comment
            }]
        );
        assert!(view.scroll.0.as_ref().borrow().base_handle.offset().y < px(0.0));
    });

    cx.simulate_keystrokes("ctrl-f");
    cx.simulate_resize(size(px(900.0), px(600.0)));
    cx.simulate_input("SELECT");
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    cx.simulate_resize(size(px(901.0), px(600.0)));
    preview.read_with(cx, |view, _| {
        assert_eq!(view.search.results.matches.len(), 100);
        let hit = view.search.active_match().expect("注释中的文本也应能搜索");
        let FilePreviewBody::Text { highlighter, .. } = &view.body else {
            panic!("Java 文件应保留文档高亮器");
        };
        let highlights = merge_log_line_highlights(
            highlighter.highlight_line(hit.line),
            None,
            Some(std::iter::once(hit.range.clone()).collect()),
            Some(hit.range.clone()),
            &view.theme,
        );
        assert_eq!(highlights.len(), 3, "搜索只覆盖命中，注释前后仍保留语法色");
        assert!(highlights[0].1.background_color.is_none());
        assert_eq!(highlights[1].0, hit.range);
        assert_eq!(
            highlights[1].1.background_color,
            Some(rgb(view.theme.warning).into())
        );
        assert_eq!(highlights[0].1.color, highlights[2].1.color);
    });
}
