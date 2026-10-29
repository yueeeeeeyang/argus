//! 文件职责：提供解析工作线程独享的有界 SQL 归一化缓存。
//! 主要功能：按原文精确命中，限制字符串字节数和条目数，避免共享锁及无界内存增长。

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use anyhow::Result;

use super::normalize_runtime_sql_structure;

/// 每个线程最多保留 2 MiB SQL 字符串和 4096 个条目；容器开销由条目数额外约束。
const MAX_CACHE_BYTES: usize = 2 * 1024 * 1024;
const MAX_CACHE_ENTRIES: usize = 4096;

/// 原文完全相同才共享解析结果；FIFO 淘汰使缓存命中不需要遍历队列。
#[derive(Debug)]
pub(super) struct RuntimeSqlCache {
    entries: HashMap<Arc<str>, Arc<str>>,
    insertion_order: VecDeque<Arc<str>>,
    string_bytes: usize,
    max_bytes: usize,
    max_entries: usize,
}

impl Default for RuntimeSqlCache {
    fn default() -> Self {
        Self::with_limits(MAX_CACHE_BYTES, MAX_CACHE_ENTRIES)
    }
}

impl RuntimeSqlCache {
    /// 创建受双重容量限制的缓存；零容量允许关闭缓存而不改变归一化语义。
    fn with_limits(max_bytes: usize, max_entries: usize) -> Self {
        Self {
            entries: HashMap::new(),
            insertion_order: VecDeque::new(),
            string_bytes: 0,
            max_bytes,
            max_entries,
        }
    }

    /// 返回原文对应的结构文本；词法错误不缓存，超大 SQL 正常解析但不占用缓存。
    pub(super) fn normalize(&mut self, sql: &str) -> Result<String> {
        self.normalize_with(sql, normalize_runtime_sql_structure)
    }

    /// 注入归一化函数便于验证精确命中和失败路径，生产环境只使用统一的 SQL 归一化器。
    fn normalize_with(
        &mut self,
        sql: &str,
        normalize: impl FnOnce(&str) -> Result<String>,
    ) -> Result<String> {
        if let Some(normalized) = self.entries.get(sql) {
            return Ok(normalized.to_string());
        }
        let normalized = normalize(sql)?;
        let required_bytes = sql.len().saturating_add(normalized.len());
        if self.max_entries == 0 || self.max_bytes == 0 || required_bytes > self.max_bytes {
            return Ok(normalized);
        }
        while self.entries.len() >= self.max_entries
            || self.string_bytes > self.max_bytes - required_bytes
        {
            let oldest = self
                .insertion_order
                .pop_front()
                .expect("非空 SQL 缓存必须有对应淘汰队列");
            let previous = self
                .entries
                .remove(&oldest)
                .expect("SQL 缓存淘汰队列必须与条目一致");
            self.string_bytes -= oldest.len() + previous.len();
        }
        let key: Arc<str> = Arc::from(sql);
        self.entries
            .insert(key.clone(), Arc::from(normalized.as_str()));
        self.insertion_order.push_back(key);
        self.string_bytes += required_bytes;
        Ok(normalized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 精确命中复用结构文本，不把仅参数不同的 SQL 当成相同原文。
    #[test]
    fn exact_hits_skip_normalization() {
        let mut cache = RuntimeSqlCache::default();
        assert_eq!(cache.normalize("SELECT 1").unwrap(), "select ?");
        let hit = cache
            .normalize_with("SELECT 1", |_| panic!("缓存命中不能重新解析"))
            .unwrap();
        assert_eq!(hit, "select ?");
        assert_eq!(cache.normalize("SELECT 2").unwrap(), "select ?");
        assert_eq!(cache.entries.len(), 2);
    }

    /// 字节和条目任一达到上限均淘汰最早条目，命中不移动队列也不扩大容量。
    #[test]
    fn both_limits_bound_fifo_cache() {
        let mut cache = RuntimeSqlCache::with_limits(8, 2);
        for sql in ["aa", "bb"] {
            cache
                .normalize_with(sql, |text| Ok(text.to_owned()))
                .unwrap();
        }
        cache.normalize_with("aa", |_| panic!("应命中")).unwrap();
        cache
            .normalize_with("cc", |text| Ok(text.to_owned()))
            .unwrap();
        assert!(!cache.entries.contains_key("aa"));
        assert_eq!(cache.entries.len(), 2);
        assert_eq!(cache.string_bytes, 8);
        cache
            .normalize_with("ddd", |text| Ok(text.to_owned()))
            .unwrap();
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.string_bytes, 6);
        let mut count_limited = RuntimeSqlCache::with_limits(100, 1);
        count_limited.normalize("select 1").unwrap();
        count_limited.normalize("select 2").unwrap();
        assert_eq!(count_limited.entries.len(), 1);
        assert!(count_limited.entries.contains_key("select 2"));
    }

    /// 超大 SQL、关闭缓存和词法失败不会清除已有结果，也不会缓存错误。
    #[test]
    fn oversized_and_invalid_sql_do_not_enter_cache() {
        let mut cache = RuntimeSqlCache::with_limits(4, 2);
        cache
            .normalize_with("a", |text| Ok(text.to_owned()))
            .unwrap();
        assert_eq!(cache.normalize("select 12345").unwrap(), "select ?");
        assert!(cache.normalize("select '").is_err());
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.string_bytes, 2);
        let mut disabled = RuntimeSqlCache::with_limits(100, 0);
        assert_eq!(disabled.normalize("select 1").unwrap(), "select ?");
        assert!(disabled.entries.is_empty());
        let mut no_bytes = RuntimeSqlCache::with_limits(0, 1);
        assert_eq!(no_bytes.normalize("").unwrap(), "");
        assert!(no_bytes.entries.is_empty());
    }
}
