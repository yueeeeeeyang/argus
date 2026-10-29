//! 文件职责：提供日志与文件预览语法高亮结果的有界行缓存。
//! 创建日期：2026-06-11
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：按阅读视图缓存最近可见行的高亮结果，降低日志或代码滚动时的重复扫描成本。

use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use crate::highlight::document::DocumentHighlighter;
use crate::highlight::highlighter::SyntaxHighlighter;
use crate::highlight::language::HighlightLanguage;
use crate::highlight::span::HighlightSpan;

/// 日志阅读区制表符展开宽度；必须与 `app::constants` 中的展示宽度一致。
const DISPLAY_TAB_SPACES: &str = "    ";

/// 单个日志标签或文件预览窗口默认缓存的高亮行数。
const DEFAULT_HIGHLIGHT_CACHE_CAPACITY: usize = 2048;

/// 单个阅读视图的语法高亮缓存，滚动时避免重复扫描热点可见行。
#[derive(Clone, Debug)]
pub(crate) struct HighlightCache {
    /// 内部可变缓存；GPUI 渲染路径通常只持有不可变 app 引用。
    inner: Arc<Mutex<HighlightCacheInner>>,
}

impl HighlightCache {
    /// 创建指定容量的高亮缓存。
    ///
    /// 参数说明：
    /// - `capacity`：最多保留的行高亮结果数量。
    ///
    /// 返回值：可克隆且内部共享的高亮缓存。
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HighlightCacheInner {
                capacity: capacity.max(1),
                entries: HashMap::new(),
                order: VecDeque::new(),
                document: None,
            })),
        }
    }

    /// 读取或生成指定行的高亮结果。
    ///
    /// 参数说明：
    /// - `line_number`：0 基内容行号。
    /// - `language`：当前阅读视图的高亮语言。
    /// - `line`：当前展示行文本。
    ///
    /// 返回值：当前行的高亮范围；缓存锁异常时直接同步计算。
    pub(crate) fn highlight_line(
        &self,
        line_number: usize,
        language: HighlightLanguage,
        line: &str,
    ) -> Vec<HighlightSpan> {
        let key = HighlightCacheKey {
            line_number,
            language,
            text_hash: stable_text_hash(line),
        };

        self.highlight_with_key(key, || SyntaxHighlighter::highlight(line, language))
    }

    /// 为内存日志按文档级词法状态生成指定行高亮。
    ///
    /// 参数说明：
    /// - `lines`：内存文档的原始行列表；缓存按 `Arc` 身份复用，不重复构建状态。
    /// - `language`：当前阅读视图的高亮语言。
    /// - `line_number`：0 基行号。
    ///
    /// 返回值：基于完整展示行生成的 token；调用方按可见切片裁剪。
    pub(crate) fn highlight_document_line(
        &self,
        lines: Arc<Vec<String>>,
        language: HighlightLanguage,
        line_number: usize,
    ) -> Vec<HighlightSpan> {
        let highlighter = {
            let Ok(mut inner) = self.inner.lock() else {
                return DocumentHighlighter::new(lines, language).highlight_line(line_number);
            };
            let needs_rebuild = inner.document.as_ref().is_none_or(|document| {
                document.language != language || !Arc::ptr_eq(&document.lines, &lines)
            });
            if needs_rebuild {
                let display_lines = Arc::new(
                    lines
                        .iter()
                        .map(|line| {
                            if line.contains('\t') {
                                line.replace('\t', DISPLAY_TAB_SPACES)
                            } else {
                                line.clone()
                            }
                        })
                        .collect::<Vec<_>>(),
                );
                inner.document = Some(DocumentHighlightCache {
                    lines,
                    language,
                    highlighter: DocumentHighlighter::new(display_lines, language),
                });
            }
            inner
                .document
                .as_ref()
                .expect("文档高亮缓存刚完成初始化")
                .highlighter
                .clone()
        };
        highlighter.highlight_line(line_number)
    }

    /// 不可变文档索引拥有独立缓存，行号已唯一绑定文本及进入状态，无需每帧重新哈希整行。
    pub(super) fn highlight_indexed_line(
        &self,
        line_number: usize,
        language: HighlightLanguage,
        compute: impl FnOnce() -> Vec<HighlightSpan>,
    ) -> Vec<HighlightSpan> {
        self.highlight_with_key(
            HighlightCacheKey {
                line_number,
                language,
                text_hash: 0,
            },
            compute,
        )
    }

    fn highlight_with_key(
        &self,
        key: HighlightCacheKey,
        compute: impl FnOnce() -> Vec<HighlightSpan>,
    ) -> Vec<HighlightSpan> {
        let Ok(inner) = self.inner.lock() else {
            return compute();
        };
        if let Some(spans) = inner.entries.get(&key).cloned() {
            return spans;
        }
        drop(inner);

        let spans = compute();
        if let Ok(mut inner) = self.inner.lock() {
            inner.insert(key, spans.clone());
        }
        spans
    }

    /// 只读取缓存中的高亮结果；缓存缺失时不执行同步高亮计算。
    pub(crate) fn cached_highlight_line(
        &self,
        line_number: usize,
        language: HighlightLanguage,
        line: &str,
    ) -> Option<Vec<HighlightSpan>> {
        let key = HighlightCacheKey {
            line_number,
            language,
            text_hash: stable_text_hash(line),
        };

        self.inner
            .lock()
            .ok()
            .and_then(|inner| inner.entries.get(&key).cloned())
    }
}

impl Default for HighlightCache {
    /// 创建默认容量的高亮缓存。
    fn default() -> Self {
        Self::with_capacity(DEFAULT_HIGHLIGHT_CACHE_CAPACITY)
    }
}

/// 高亮缓存键，使用文本 hash 处理同一行内容变化的情况。
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct HighlightCacheKey {
    /// 0 基日志行号。
    line_number: usize,
    /// 当前行使用的高亮语言。
    language: HighlightLanguage,
    /// 展示文本的稳定 hash。
    text_hash: u64,
}

/// 高亮缓存内部结构。
#[derive(Debug)]
struct HighlightCacheInner {
    /// 最大缓存行数。
    capacity: usize,
    /// 缓存键到高亮范围的映射。
    entries: HashMap<HighlightCacheKey, Vec<HighlightSpan>>,
    /// 插入顺序，队尾是最新加入的缓存项。
    order: VecDeque<HighlightCacheKey>,
    /// 内存日志的文档级词法高亮器；分页日志不进入该路径。
    document: Option<DocumentHighlightCache>,
}

/// 内存日志文档高亮缓存。
#[derive(Debug)]
struct DocumentHighlightCache {
    /// 原始行列表，用于识别文档是否发生变化。
    lines: Arc<Vec<String>>,
    /// 当前高亮语言。
    language: HighlightLanguage,
    /// 已建立全文进入状态的高亮器。
    highlighter: DocumentHighlighter,
}

impl HighlightCacheInner {
    /// 插入缓存并淘汰最早加入的行。
    fn insert(&mut self, key: HighlightCacheKey, spans: Vec<HighlightSpan>) {
        if self.entries.insert(key, spans).is_none() {
            self.order.push_back(key);
        }
        while self.entries.len() > self.capacity {
            let Some(old_key) = self.order.pop_front() else {
                break;
            };
            self.entries.remove(&old_key);
        }
    }
}

/// 构造稳定文本 hash，避免把整行内容放进缓存键。
fn stable_text_hash(text: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::highlight::span::HighlightTokenKind;

    /// 内存日志使用文档级缓存后，XML 多行注释必须跨行连续高亮。
    #[test]
    fn document_cache_preserves_xml_block_comment_state() {
        assert_eq!(
            DISPLAY_TAB_SPACES,
            crate::app::LOG_VIEWER_TAB_DISPLAY_SPACES,
            "高亮缓存与日志展示必须使用相同制表符宽度"
        );
        let lines = Arc::new(
            vec!["<root>", "<!--", "  multi-line comment", "-->", "<node/>"]
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>(),
        );
        let cache = HighlightCache::default();

        let comment = cache.highlight_document_line(lines.clone(), HighlightLanguage::Xml, 2);
        assert!(
            comment
                .iter()
                .any(|span| span.kind == HighlightTokenKind::Comment
                    && span.range == (0..lines[2].len())),
            "注释中间行应整体标记为注释：{comment:?}"
        );

        let closing = cache.highlight_document_line(lines.clone(), HighlightLanguage::Xml, 3);
        assert!(
            closing
                .iter()
                .any(|span| span.kind == HighlightTokenKind::Comment),
            "注释闭合行应继续识别为注释：{closing:?}"
        );

        let next = cache.highlight_document_line(lines, HighlightLanguage::Xml, 4);
        assert!(
            next.iter()
                .all(|span| span.kind != HighlightTokenKind::Comment),
            "注释闭合后代码不应继续标记为注释：{next:?}"
        );
    }
}
