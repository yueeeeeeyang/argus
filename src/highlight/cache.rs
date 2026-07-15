//! 文件职责：提供日志与文件预览语法高亮结果的轻量 LRU 缓存。
//! 创建日期：2026-06-11
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：按阅读视图缓存最近可见行的高亮结果，降低日志或代码滚动时的重复扫描成本。

use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use crate::highlight::highlighter::SyntaxHighlighter;
use crate::highlight::language::HighlightLanguage;
use crate::highlight::span::HighlightSpan;

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

        let Ok(inner) = self.inner.lock() else {
            return SyntaxHighlighter::highlight(line, language);
        };
        if let Some(spans) = inner.entries.get(&key).cloned() {
            return spans;
        }
        drop(inner);

        let spans = SyntaxHighlighter::highlight(line, language);
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
    /// 访问顺序，队尾是最新访问。
    order: VecDeque<HighlightCacheKey>,
}

impl HighlightCacheInner {
    /// 插入缓存并淘汰最久未使用的行。
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
