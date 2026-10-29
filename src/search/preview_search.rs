//! 文件职责：对已加载的文件预览文本执行有界、可取消的字面量搜索。
//! 主要功能：复用日志匹配规则，保留原始 UTF-8 字节范围，并提供逐行命中查询。

use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};

use super::search_engine::{SearchMatcher, SearchQuery};

/// 预览只保留有限命中用于高亮和导航，超过上限时由界面明确提示。
pub(crate) const PREVIEW_SEARCH_MATCH_LIMIT: usize = 20_000;
/// 常规关键词的正文窗口；长关键词使用不小于关键词长度的窗口以限制重复扫描。
const SEARCH_WINDOW_CHARACTERS: usize = 4096;

/// 单个预览命中，位置基于传入的原始行文本。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PreviewSearchMatch {
    /// 从零开始的文档行索引。
    pub line: usize,
    /// 行内 UTF-8 字节范围，可直接用于切片及文本高亮。
    pub range: Range<usize>,
}

/// 按行号、行内位置排序的预览搜索结果。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct PreviewSearchResults {
    /// 最多保留 `PREVIEW_SEARCH_MATCH_LIMIT` 项非重叠命中。
    pub matches: Vec<PreviewSearchMatch>,
    /// 已确认还有未保留的命中；恰好达到上限时仍为 false。
    pub is_truncated: bool,
}

impl PreviewSearchResults {
    /// 二分定位一行的全部命中，重绘时无需扫描整份搜索结果。
    pub(crate) fn matches_for_line(&self, line: usize) -> &[PreviewSearchMatch] {
        let start = self.matches.partition_point(|item| item.line < line);
        let end = self.matches[start..].partition_point(|item| item.line == line) + start;
        &self.matches[start..end]
    }
}

/// 搜索已加载的预览行；`case_sensitive=false` 沿用日志搜索的 Unicode 小写匹配规则。
///
/// 空关键词返回成功空结果；取消返回 `Ok(None)`，调用方不得发布部分结果。
/// 匹配器构造失败时返回中文错误。关键词始终按字面量处理，范围基于输入行的原始字节。
pub(crate) fn search_preview_lines(
    lines: &[String],
    keyword: &str,
    case_sensitive: bool,
    cancel: &AtomicBool,
) -> Result<Option<PreviewSearchResults>, String> {
    search_preview_lines_with_cancellation(lines, keyword, case_sensitive, || {
        cancel.load(Ordering::Relaxed)
    })
}

/// 注入取消检查，允许测试在指定窗口或命中处取消，避免依赖线程时序。
fn search_preview_lines_with_cancellation(
    lines: &[String],
    keyword: &str,
    case_sensitive: bool,
    mut is_cancelled: impl FnMut() -> bool,
) -> Result<Option<PreviewSearchResults>, String> {
    if is_cancelled() {
        return Ok(None);
    }
    let mut results = PreviewSearchResults::default();
    if keyword.is_empty() {
        return Ok(Some(results));
    }
    let matcher = SearchMatcher::new(&SearchQuery {
        keyword: keyword.to_string(),
        case_sensitive,
        regex_enabled: false,
    })?;
    // 一个原始字符至少产生一个小写字符，因此该数量足以覆盖跨窗口的完整字面量。
    // 必须按字符而非关键词字节长度计算，才能处理 İ 等小写后长度变化的字符。
    let keyword_characters = if case_sensitive {
        keyword.chars().count()
    } else {
        keyword.chars().flat_map(char::to_lowercase).count()
    };
    let overlap_characters = keyword_characters.saturating_sub(1);
    // 正文窗口不能短于边界上下文，否则长关键词会让同一段文本被反复扫描。
    // 上下文小于正文窗口时，每个输入字符最多进入两个相邻匹配窗口。
    let window_characters = SEARCH_WINDOW_CHARACTERS.max(keyword_characters);

    for (line_index, line) in lines.iter().enumerate() {
        if is_cancelled() {
            return Ok(None);
        }
        let mut search_from = 0;
        while search_from < line.len() {
            if is_cancelled() {
                return Ok(None);
            }
            let main_end = byte_boundary_after(line, search_from, window_characters);
            let window_end = byte_boundary_after(line, main_end, overlap_characters);
            let mut next_search_from = main_end;
            if let Some(ranges) = matcher.find_match_ranges(&line[search_from..window_end]) {
                for range in ranges {
                    if is_cancelled() {
                        return Ok(None);
                    }
                    let start = search_from + range.start;
                    if start >= main_end {
                        break;
                    }
                    if results.matches.len() == PREVIEW_SEARCH_MATCH_LIMIT {
                        results.is_truncated = true;
                        return Ok(Some(results));
                    }
                    let end = search_from + range.end;
                    results.matches.push(PreviewSearchMatch {
                        line: line_index,
                        range: start..end,
                    });
                    // 跨界命中已经消费的字符不能成为下一窗口的起点，保持非重叠匹配语义。
                    next_search_from = next_search_from.max(end);
                }
            }
            search_from = next_search_from;
        }
    }
    if is_cancelled() {
        Ok(None)
    } else {
        Ok(Some(results))
    }
}

/// 返回从合法字节边界前进指定字符数的位置；不足时停在行末。
fn byte_boundary_after(line: &str, start: usize, characters: usize) -> usize {
    line[start..]
        .char_indices()
        .nth(characters)
        .map_or(line.len(), |(offset, _)| start + offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn search(lines: &[&str], keyword: &str, case_sensitive: bool) -> PreviewSearchResults {
        search_preview_lines(
            &lines
                .iter()
                .map(|line| (*line).to_owned())
                .collect::<Vec<_>>(),
            keyword,
            case_sensitive,
            &AtomicBool::new(false),
        )
        .expect("字面量搜索应能构造匹配器")
        .expect("搜索未取消")
    }

    /// 默认大小写规则与日志一致，返回所有命中且保持文档顺序。
    #[test]
    fn finds_all_matches_in_document_order() {
        let results = search(&["ERROR warn error", "", "error ERROR"], "error", false);
        assert_eq!(
            results.matches,
            vec![
                PreviewSearchMatch {
                    line: 0,
                    range: 0..5
                },
                PreviewSearchMatch {
                    line: 0,
                    range: 11..16
                },
                PreviewSearchMatch {
                    line: 2,
                    range: 0..5
                },
                PreviewSearchMatch {
                    line: 2,
                    range: 6..11
                },
            ]
        );
        assert!(!results.is_truncated);
        assert_eq!(
            search(&["ERROR error Error"], "error", true).matches,
            vec![PreviewSearchMatch {
                line: 0,
                range: 6..11
            }]
        );
    }

    /// Unicode 命中范围始终指向原始 UTF-8 字节，包括小写展开成多个字符的情况。
    #[test]
    fn preserves_unicode_byte_ranges() {
        let line = "前İ i\u{307} 后中文";
        let results = search(&[line], "İ", false);
        assert_eq!(
            results.matches,
            vec![
                PreviewSearchMatch {
                    line: 0,
                    range: 3..5
                },
                PreviewSearchMatch {
                    line: 0,
                    range: 6..9
                },
            ]
        );
        assert_eq!(&line[results.matches[0].range.clone()], "İ");
        assert_eq!(&line[results.matches[1].range.clone()], "i\u{307}");
        let chinese = search(&[line], "中文", false);
        assert_eq!(&line[chinese.matches[0].range.clone()], "中文");
        assert!(search(&["İ"], "i", false).matches.is_empty());
    }

    /// 空输入及无结果成功返回空集合；空格和正则标点仍按原始字面量搜索。
    #[test]
    fn handles_empty_missing_and_literal_queries() {
        for results in [
            search(&[], "x", false),
            search(&["abc"], "", false),
            search(&["abc", ""], "missing", false),
        ] {
            assert_eq!(results, PreviewSearchResults::default());
        }
        assert_eq!(search(&["a [.*] b"], "[.*]", false).matches[0].range, 2..6);
        assert_eq!(search(&["a b"], " ", false).matches[0].range, 1..2);
        assert_eq!(
            search(&["aaaaa"], "aa", false)
                .matches
                .iter()
                .map(|item| item.range.clone())
                .collect::<Vec<_>>(),
            vec![0..2, 2..4]
        );
    }

    /// 分窗口搜索必须与原匹配器完全相同，涵盖跨边界、Unicode 展开和非重叠推进。
    #[test]
    fn chunk_boundaries_preserve_existing_matcher_semantics() {
        let cases = [
            (
                format!("{}abcabc", "x".repeat(SEARCH_WINDOW_CHARACTERS - 1)),
                "abc",
                false,
            ),
            (
                format!("{}i\u{307}İ", "中".repeat(SEARCH_WINDOW_CHARACTERS - 1)),
                "İ",
                false,
            ),
            ("a".repeat(SEARCH_WINDOW_CHARACTERS * 3 + 2), "aaa", true),
            ("Aa".repeat(SEARCH_WINDOW_CHARACTERS * 2), "aAa", false),
            (
                format!("{}{}", "x".repeat(SEARCH_WINDOW_CHARACTERS - 1), "中文中文"),
                "中文",
                true,
            ),
        ];
        for (line, keyword, case_sensitive) in cases {
            let matcher = SearchMatcher::new(&SearchQuery {
                keyword: keyword.into(),
                case_sensitive,
                regex_enabled: false,
            })
            .unwrap();
            let expected = matcher.find_match_ranges(&line).unwrap_or_default();
            let actual = search(&[&line], keyword, case_sensitive);
            assert_eq!(
                actual
                    .matches
                    .iter()
                    .map(|item| item.range.clone())
                    .collect::<Vec<_>>(),
                expected
            );
            assert!(!actual.is_truncated);
        }
    }

    /// 确定性组合多个窗口边界与 Unicode 大小写变体，逐项对照原匹配器的完整行结果。
    #[test]
    fn generated_unicode_boundaries_match_existing_matcher() {
        let examples = [
            ("İ", "i\u{307} İ i"),
            ("i\u{307}", "İ i\u{307} i"),
            ("k", "K K k"),
            ("Σσ", "ΣΣ σσ Σς"),
            ("ß", "ẞ ß"),
            ("😀中", "😀中😀中"),
            ("AaA", "aAaAaA"),
            ("a.a", "A.A a.a"),
        ];
        for boundary in [SEARCH_WINDOW_CHARACTERS, SEARCH_WINDOW_CHARACTERS * 2] {
            for prefix_characters in boundary - 1..=boundary + 1 {
                for (keyword, body) in examples {
                    let line = format!("{}{} {}", "文".repeat(prefix_characters), body, body);
                    for case_sensitive in [false, true] {
                        assert_matches_existing_matcher(&line, keyword, case_sensitive);
                    }
                }
            }
        }
    }

    /// 长于基础窗口的关键词仍跨边界完整匹配，Unicode 小写展开也参与窗口长度计算。
    #[test]
    fn long_keywords_preserve_cross_window_matches() {
        for keyword_characters in [
            SEARCH_WINDOW_CHARACTERS - 1,
            SEARCH_WINDOW_CHARACTERS,
            SEARCH_WINDOW_CHARACTERS + 1,
            SEARCH_WINDOW_CHARACTERS * 2 + 3,
        ] {
            let keyword = format!("{}Z", "a".repeat(keyword_characters - 1));
            let window_characters = SEARCH_WINDOW_CHARACTERS.max(keyword_characters);
            let line = format!(
                "{}{} {}",
                "x".repeat(window_characters - 1),
                keyword,
                keyword
            );
            for case_sensitive in [false, true] {
                assert_matches_existing_matcher(&line, &keyword, case_sensitive);
            }
            assert_matches_existing_matcher("short", &keyword, false);
        }
        let repetitions = SEARCH_WINDOW_CHARACTERS + 1;
        let keyword = "İ".repeat(repetitions);
        let expanded = "i\u{307}".repeat(repetitions);
        let line = format!(
            "{}{} {}",
            "文".repeat(repetitions * 2 - 1),
            expanded,
            expanded
        );
        assert_matches_existing_matcher(&line, &keyword, false);
    }

    /// 无命中时窗口数随正文长度除以关键词长度增长，不能固定步长重复扫描长上下文。
    #[test]
    fn long_keyword_overlap_has_linear_window_work() {
        let keyword_characters = SEARCH_WINDOW_CHARACTERS * 16 + 1;
        let keyword = format!("{}b", "a".repeat(keyword_characters - 1));
        let line = "a".repeat(keyword_characters * 4 + 1);
        let mut checks = 0;
        let result = search_preview_lines_with_cancellation(
            std::slice::from_ref(&line),
            &keyword,
            false,
            || {
                checks += 1;
                false
            },
        )
        .unwrap()
        .unwrap();
        assert!(result.matches.is_empty());
        assert!(!result.is_truncated);
        // 无命中时只有入口、行入口、每个窗口和结束检查，不受墙钟时间或机器负载影响。
        assert_eq!(checks, line.len().div_ceil(keyword_characters) + 3);
    }

    fn assert_matches_existing_matcher(line: &str, keyword: &str, case_sensitive: bool) {
        let matcher = SearchMatcher::new(&SearchQuery {
            keyword: keyword.into(),
            case_sensitive,
            regex_enabled: false,
        })
        .unwrap();
        let expected = matcher.find_match_ranges(line).unwrap_or_default();
        let actual = search(&[line], keyword, case_sensitive);
        assert_eq!(
            actual
                .matches
                .iter()
                .map(|item| item.range.clone())
                .collect::<Vec<_>>(),
            expected,
            "行字符数={}，关键词字符数={}，区分大小写={case_sensitive}",
            line.chars().count(),
            keyword.chars().count()
        );
        assert!(
            actual
                .matches
                .iter()
                .all(|item| line.is_char_boundary(item.range.start)
                    && line.is_char_boundary(item.range.end))
        );
        assert!(!actual.is_truncated);
    }

    /// 达到上限不等于截断，只有确认额外命中时才提示截断，单行超多命中也受限。
    #[test]
    fn distinguishes_exact_limit_from_truncated_results() {
        let exact_line = "x".repeat(PREVIEW_SEARCH_MATCH_LIMIT);
        let exact = search(&[&exact_line, "unmatched"], "x", true);
        assert_eq!(exact.matches.len(), PREVIEW_SEARCH_MATCH_LIMIT);
        assert!(!exact.is_truncated);
        for lines in [
            vec![format!("{exact_line}x")],
            vec![exact_line.clone(), "x".into()],
        ] {
            let truncated = search_preview_lines(&lines, "x", true, &AtomicBool::new(false))
                .unwrap()
                .unwrap();
            assert_eq!(truncated.matches.len(), PREVIEW_SEARCH_MATCH_LIMIT);
            assert!(truncated.is_truncated);
            assert_eq!(
                truncated.matches.last().unwrap().range,
                PREVIEW_SEARCH_MATCH_LIMIT - 1..PREVIEW_SEARCH_MATCH_LIMIT
            );
        }
    }

    /// 逐行查询只返回当前行切片，空行、开头之前和末尾之后均返回空切片。
    #[test]
    fn indexes_line_matches_without_cloning() {
        let results = search(&["", "x x", "", "x", ""], "x", true);
        assert_eq!(results.matches_for_line(1), &results.matches[0..2]);
        assert_eq!(results.matches_for_line(3), &results.matches[2..3]);
        for line in [0, 2, 4, usize::MAX] {
            assert!(results.matches_for_line(line).is_empty());
        }
        assert!(
            PreviewSearchResults::default()
                .matches_for_line(0)
                .is_empty()
        );
    }

    /// 提前取消和命中积累期间取消均丢弃部分结果，不能作为成功空结果发布。
    #[test]
    fn cancellation_discards_partial_results() {
        let cancelled = AtomicBool::new(true);
        assert!(
            search_preview_lines(&["x".into()], "x", false, &cancelled)
                .unwrap()
                .is_none()
        );
        assert!(
            search_preview_lines(&[], "", false, &cancelled)
                .unwrap()
                .is_none()
        );
        let mut checks = 0;
        let result = search_preview_lines_with_cancellation(&["x".repeat(100)], "x", false, || {
            checks += 1;
            checks == 8
        })
        .unwrap();
        assert!(result.is_none());
        assert_eq!(checks, 8);
    }

    /// 超长且无命中的行也在窗口间检查取消，不必等整行扫描结束。
    #[test]
    fn long_unmatched_lines_remain_cancellable() {
        let mut checks = 0;
        let result = search_preview_lines_with_cancellation(
            &["a".repeat(SEARCH_WINDOW_CHARACTERS * 4)],
            "z",
            false,
            || {
                checks += 1;
                checks == 4
            },
        )
        .unwrap();
        assert!(result.is_none());
        assert_eq!(checks, 4);
    }
}
