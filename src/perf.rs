//! 文件职责：提供轻量性能打点工具。
//! 创建日期：2026-07-08
//! 修改日期：2026-07-08
//! 作者：Argus 开发团队
//! 主要功能：在调试开关开启时记录超过阈值的 UI/数据路径耗时。

use std::time::{Duration, Instant};

/// 性能打点环境变量；设为 `1` 时输出慢路径日志。
const ARGUS_PERF_ENV: &str = "ARGUS_PERF";
/// 默认慢路径阈值，避免正常短耗时刷屏。
const DEFAULT_SLOW_THRESHOLD: Duration = Duration::from_millis(16);

/// 作用域耗时打点；离开作用域时按阈值输出慢路径。
pub struct PerfSpan {
    /// 打点名称。
    label: &'static str,
    /// 起始时间。
    started_at: Instant,
    /// 输出阈值。
    threshold: Duration,
    /// 当前打点是否启用。
    enabled: bool,
}

impl PerfSpan {
    /// 创建默认阈值的性能打点。
    pub fn new(label: &'static str) -> Self {
        Self::with_threshold(label, DEFAULT_SLOW_THRESHOLD)
    }

    /// 创建指定阈值的性能打点。
    pub fn with_threshold(label: &'static str, threshold: Duration) -> Self {
        Self {
            label,
            started_at: Instant::now(),
            threshold,
            enabled: std::env::var(ARGUS_PERF_ENV).is_ok_and(|value| value == "1"),
        }
    }
}

impl Drop for PerfSpan {
    /// 作用域结束时输出超过阈值的耗时。
    fn drop(&mut self) {
        if !self.enabled {
            return;
        }

        let elapsed = self.started_at.elapsed();
        if elapsed >= self.threshold {
            eprintln!("[argus perf] {} took {:.2?}", self.label, elapsed);
        }
    }
}
