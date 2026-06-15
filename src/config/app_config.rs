//! 文件职责：定义应用运行期配置与持久化设置模型。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-15
//! 作者：Argus 开发团队
//! 主要功能：提供外观、日志加载、日志搜索、编码和缓存设置的默认值、校验和 TOML 序列化结构。

use serde::{Deserialize, Serialize};

/// 应用配置根对象，字段结构与 `~/.argus/settings.toml` 保持一致。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AppConfig {
    /// 外观配置，控制主题文件选择和日志阅读区域字号。
    #[serde(default)]
    pub appearance: AppearanceConfig,
    /// 日志来源加载配置，控制目录和压缩包的展开策略。
    #[serde(default)]
    pub loader: LoaderConfig,
    /// 日志搜索配置，保存快搜关键字等跨会话搜索偏好。
    #[serde(default)]
    pub log_search: LogSearchConfig,
    /// 编码配置，后续日志读取模块会据此选择默认解码策略。
    #[serde(default)]
    pub encoding: EncodingConfig,
    /// 缓存配置，后续索引和读取模块会据此控制临时缓存策略。
    #[serde(default)]
    pub cache: CacheConfig,
}

impl AppConfig {
    /// 返回经过边界修正的配置副本。
    ///
    /// 返回值：所有数值型配置均被限制在当前 UI 可展示范围内，避免坏配置破坏界面状态。
    pub fn normalized(mut self) -> Self {
        self.appearance.theme_mode = match self.appearance.theme_mode.trim() {
            "" => "dark.toml".to_string(),
            value
                if matches!(
                    value.to_ascii_lowercase().as_str(),
                    "system" | "light" | "dark"
                ) =>
            {
                "dark.toml".to_string()
            }
            value => value.to_string(),
        };
        self.appearance.log_content_font_size =
            self.appearance.log_content_font_size.clamp(12.0, 20.0);
        self.loader.max_archive_depth = self.loader.max_archive_depth.min(8);
        self.loader.archive_probe_concurrency = self.loader.archive_probe_concurrency.clamp(1, 16);
        self.log_search.quick_keywords = self.log_search.quick_keywords.trim().to_string();
        self.cache.limit_mb = self.cache.limit_mb.clamp(128, 2048);
        if self.encoding.selected.trim().is_empty() {
            self.encoding.selected = EncodingConfig::default().selected;
        }
        self
    }
}

impl Default for AppConfig {
    /// 构造应用默认配置，保证无设置文件时也能稳定启动。
    fn default() -> Self {
        Self {
            appearance: AppearanceConfig::default(),
            loader: LoaderConfig::default(),
            log_search: LogSearchConfig::default(),
            encoding: EncodingConfig::default(),
            cache: CacheConfig::default(),
        }
    }
}

/// 外观配置，持久化设置页中的主题文件和日志内容字号。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AppearanceConfig {
    /// 主题文件标识，内置主题为 `dark.toml`，用户主题为 `~/.argus/themes` 下的 TOML 文件名。
    pub theme_mode: String,
    /// 日志内容区字号，仅影响主阅读区域和未读取提示。
    pub log_content_font_size: f32,
}

impl Default for AppearanceConfig {
    /// 构造默认外观配置，沿用当前深色主题和 12px 日志阅读字号。
    fn default() -> Self {
        Self {
            theme_mode: "dark.toml".to_string(),
            log_content_font_size: 12.0,
        }
    }
}

/// 日志来源加载配置，用于限制高成本文件系统和压缩包操作。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LoaderConfig {
    /// 允许展开的嵌套压缩包最大层级，默认 2 层。
    pub max_archive_depth: usize,
    /// 当前目录层单文件压缩包探测并发数，默认 4，避免大量压缩包串行探测过慢。
    #[serde(default = "default_archive_probe_concurrency")]
    pub archive_probe_concurrency: usize,
    /// 是否跟随符号链接；默认关闭以避免大目录扫描时出现循环。
    pub follow_symlinks: bool,
}

impl Default for LoaderConfig {
    /// 构造加载模块默认配置，保证大目录加载采用保守策略。
    fn default() -> Self {
        Self {
            max_archive_depth: 2,
            archive_probe_concurrency: default_archive_probe_concurrency(),
            follow_symlinks: false,
        }
    }
}

/// 返回默认单文件压缩包探测并发数。
fn default_archive_probe_concurrency() -> usize {
    4
}

/// 日志搜索配置，当前用于保存快搜关键字。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct LogSearchConfig {
    /// 快搜关键字原始输入，使用英文逗号分隔；解析和去重在搜索启动时执行。
    pub quick_keywords: String,
}

/// 编码配置，当前先持久化用户选择，日志正文读取接入后再参与解码。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EncodingConfig {
    /// 用户选择的默认编码名称。
    pub selected: String,
}

impl Default for EncodingConfig {
    /// 构造默认编码配置。
    fn default() -> Self {
        Self {
            selected: "UTF-8".to_string(),
        }
    }
}

/// 缓存配置，当前先持久化设置页状态，后续缓存模块接入时复用。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CacheConfig {
    /// 是否启用临时缓存。
    pub enabled: bool,
    /// 缓存上限，单位 MB。
    pub limit_mb: usize,
}

impl Default for CacheConfig {
    /// 构造默认缓存配置。
    fn default() -> Self {
        Self {
            enabled: true,
            limit_mb: 512,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证坏配置会被修正到 UI 允许的范围内。
    #[test]
    fn normalized_clamps_numeric_settings() {
        let config = AppConfig {
            appearance: AppearanceConfig {
                theme_mode: "light".to_string(),
                log_content_font_size: 99.0,
            },
            loader: LoaderConfig {
                max_archive_depth: 99,
                archive_probe_concurrency: 99,
                follow_symlinks: true,
            },
            log_search: LogSearchConfig {
                quick_keywords: " ERROR, WARN ".to_string(),
            },
            encoding: EncodingConfig {
                selected: String::new(),
            },
            cache: CacheConfig {
                enabled: true,
                limit_mb: 1,
            },
        }
        .normalized();

        assert_eq!(config.appearance.log_content_font_size, 20.0);
        assert_eq!(config.appearance.theme_mode, "dark.toml");
        assert_eq!(config.loader.max_archive_depth, 8);
        assert_eq!(config.loader.archive_probe_concurrency, 16);
        assert_eq!(config.log_search.quick_keywords, "ERROR, WARN");
        assert_eq!(config.encoding.selected, "UTF-8");
        assert_eq!(config.cache.limit_mb, 128);
    }

    /// 验证默认压缩包探测并发数为 4，兼顾展开速度和后台资源占用。
    #[test]
    fn default_archive_probe_concurrency_is_four() {
        assert_eq!(LoaderConfig::default().archive_probe_concurrency, 4);
    }

    /// 验证新安装用户默认使用设计文档要求的 12px 日志字号。
    #[test]
    fn default_log_content_font_size_is_twelve() {
        assert_eq!(AppearanceConfig::default().log_content_font_size, 12.0);
    }

    /// 验证日志搜索配置默认没有快搜关键字，避免新用户误触发搜索。
    #[test]
    fn default_quick_search_keywords_is_empty() {
        assert!(LogSearchConfig::default().quick_keywords.is_empty());
    }
}
