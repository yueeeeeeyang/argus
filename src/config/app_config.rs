//! 文件职责：定义应用运行期配置与持久化设置模型。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：提供外观、日志加载、编码和缓存设置的默认值、校验和 TOML 序列化结构。

use serde::{Deserialize, Serialize};

/// 应用配置根对象，字段结构与 `~/.argus/settings.toml` 保持一致。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AppConfig {
    /// 外观配置，控制主题模式和日志阅读区域字号。
    #[serde(default)]
    pub appearance: AppearanceConfig,
    /// 日志来源加载配置，控制目录和压缩包的展开策略。
    #[serde(default)]
    pub loader: LoaderConfig,
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
        self.appearance.theme_mode = match self.appearance.theme_mode.trim().to_ascii_lowercase() {
            value if value == "system" || value == "light" => value,
            _ => "dark".to_string(),
        };
        self.appearance.log_content_font_size =
            self.appearance.log_content_font_size.clamp(12.0, 20.0);
        self.loader.max_archive_depth = self.loader.max_archive_depth.min(8);
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
            encoding: EncodingConfig::default(),
            cache: CacheConfig::default(),
        }
    }
}

/// 外观配置，持久化设置页中的主题和日志内容字号。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AppearanceConfig {
    /// 主题模式标识，合法值为 `system`、`dark`、`light`。
    pub theme_mode: String,
    /// 日志内容区字号，仅影响主阅读区域和未读取提示。
    pub log_content_font_size: f32,
}

impl Default for AppearanceConfig {
    /// 构造默认外观配置，沿用当前深色主题和 12px 日志阅读字号。
    fn default() -> Self {
        Self {
            theme_mode: "dark".to_string(),
            log_content_font_size: 12.0,
        }
    }
}

/// 日志来源加载配置，用于限制高成本文件系统和压缩包操作。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LoaderConfig {
    /// 允许展开的嵌套压缩包最大层级，默认 2 层。
    pub max_archive_depth: usize,
    /// 是否跟随符号链接；默认关闭以避免大目录扫描时出现循环。
    pub follow_symlinks: bool,
}

impl Default for LoaderConfig {
    /// 构造加载模块默认配置，保证大目录加载采用保守策略。
    fn default() -> Self {
        Self {
            max_archive_depth: 2,
            follow_symlinks: false,
        }
    }
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
                theme_mode: "dark".to_string(),
                log_content_font_size: 99.0,
            },
            loader: LoaderConfig {
                max_archive_depth: 99,
                follow_symlinks: true,
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
        assert_eq!(config.appearance.theme_mode, "dark");
        assert_eq!(config.loader.max_archive_depth, 8);
        assert_eq!(config.encoding.selected, "UTF-8");
        assert_eq!(config.cache.limit_mb, 128);
    }

    /// 验证新安装用户默认使用设计文档要求的 12px 日志字号。
    #[test]
    fn default_log_content_font_size_is_twelve() {
        assert_eq!(AppearanceConfig::default().log_content_font_size, 12.0);
    }
}
