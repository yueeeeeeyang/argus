//! 文件职责：实现 Argus 主题文件加载与校验管理。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：从内置 TOML 和 `~/.argus/themes` 读取主题令牌，解析为运行期 AppTheme。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

use crate::config::paths::argus_theme_dir;
use crate::theme::{AppTheme, SyntaxTheme};

/// 内置深色主题 TOML 内容，使用 include_str 保证打包后仍可读取。
const BUILTIN_DARK_THEME_TOML: &str = include_str!("../../themes/dark.toml");
/// 内置浅色主题 TOML 内容，使用 include_str 保证打包后仍可读取。
const BUILTIN_LIGHT_THEME_TOML: &str = include_str!("../../themes/light.toml");
/// 深色主题模式标识。
const DARK_MODE: &str = "dark";
/// 浅色主题模式标识。
const LIGHT_MODE: &str = "light";

/// 主题加载错误，区分 TOML 解析和颜色令牌校验失败。
#[derive(Debug, Error)]
pub enum ThemeError {
    /// 主题文件 TOML 结构不合法。
    #[error("主题 TOML 解析失败：{0}")]
    Toml(#[from] toml::de::Error),
    /// 主题颜色格式不合法。
    #[error("主题颜色 `{token}` 的值 `{value}` 不合法，应使用 #RRGGBB 或 #RRGGBBAA")]
    InvalidColor {
        /// 颜色 token 名称。
        token: &'static str,
        /// 主题文件中的原始颜色值。
        value: String,
    },
    /// 主题模式不在当前支持范围内。
    #[error("主题 `{name}` 的 mode `{mode}` 不受支持")]
    UnsupportedMode {
        /// 主题名称。
        name: String,
        /// 主题模式。
        mode: String,
    },
}

/// 主题管理器，负责持有内置主题和用户主题加载警告。
#[derive(Clone, Debug)]
pub struct ThemeManager {
    /// 按模式索引的内置主题，当前设置页直接使用 `dark` 和 `light`。
    builtin_themes: HashMap<String, AppTheme>,
    /// 用户主题集合，当前阶段只加载和校验，后续设置页可扩展为可选主题列表。
    user_themes: Vec<LoadedUserTheme>,
    /// 主题加载警告，非法用户主题不会阻塞启动。
    warnings: Vec<String>,
}

/// 已加载的用户主题元信息。
#[derive(Clone, Debug)]
pub struct LoadedUserTheme {
    /// 主题名称。
    pub name: String,
    /// 主题模式。
    pub mode: String,
    /// 主题文件路径。
    pub path: PathBuf,
    /// 解析后的主题令牌。
    pub theme: AppTheme,
}

/// TOML 主题文件结构。
#[derive(Debug, Deserialize)]
struct ThemeFile {
    /// 主题名称。
    name: String,
    /// 主题模式，当前支持 dark/light。
    mode: String,
    /// 主题描述，仅用于后续设置页展示。
    #[allow(dead_code)]
    description: Option<String>,
    /// 常规界面颜色。
    colors: ThemeColorTokens,
    /// 日志级别与状态颜色。
    log_levels: ThemeLogLevelTokens,
    /// 语法高亮颜色；用户旧主题缺失时使用对应主题模式默认值。
    syntax: Option<ThemeSyntaxTokens>,
}

/// TOML 中的界面颜色 token。
#[derive(Debug, Deserialize)]
struct ThemeColorTokens {
    background: String,
    title_bar: String,
    activity_bar: String,
    side_bar: String,
    content: String,
    status_bar: String,
    foreground: String,
    foreground_muted: String,
    border: String,
    selection: String,
    current_line: String,
    modal_overlay: String,
}

/// TOML 中的日志级别颜色 token。
#[derive(Debug, Deserialize)]
struct ThemeLogLevelTokens {
    debug: String,
    info: String,
    warning: String,
    error: String,
    success: String,
}

/// TOML 中的语法高亮 token；字段可选以兼容用户旧主题。
#[derive(Debug, Default, Deserialize)]
struct ThemeSyntaxTokens {
    comment: Option<String>,
    key: Option<String>,
    string: Option<String>,
    number: Option<String>,
    boolean: Option<String>,
    punctuation: Option<String>,
    tag: Option<String>,
    attribute: Option<String>,
    timestamp: Option<String>,
    thread: Option<String>,
    class: Option<String>,
    method: Option<String>,
    lock: Option<String>,
    exception: Option<String>,
}

impl ThemeManager {
    /// 加载默认主题目录下的内置主题和用户主题。
    ///
    /// 返回值：始终返回可用管理器；内置主题解析失败时使用紧急 fallback，用户主题失败则记录 warning。
    pub fn load_default() -> Self {
        Self::load_with_user_theme_dir(&argus_theme_dir())
    }

    /// 使用指定用户主题目录加载主题，便于单元测试隔离真实用户目录。
    pub fn load_with_user_theme_dir(user_theme_dir: &Path) -> Self {
        let mut manager = Self {
            builtin_themes: Self::load_builtin_themes(),
            user_themes: Vec::new(),
            warnings: Vec::new(),
        };
        manager.load_user_themes(user_theme_dir);
        manager
    }

    /// 返回指定模式对应的主题，模式未知时回退深色主题。
    pub fn theme_for_mode(&self, mode: &str) -> AppTheme {
        let normalized_mode = normalize_mode(mode);
        self.builtin_themes
            .get(normalized_mode)
            .or_else(|| self.builtin_themes.get(DARK_MODE))
            .cloned()
            .unwrap_or_else(AppTheme::dark)
    }

    /// 返回用户主题加载警告。
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// 返回已经通过校验的用户主题列表。
    pub fn user_themes(&self) -> &[LoadedUserTheme] {
        &self.user_themes
    }

    /// 从内置深色 TOML 构造主题，供旧 builtin 模块保持兼容。
    pub fn builtin_dark_theme() -> AppTheme {
        parse_theme_toml(BUILTIN_DARK_THEME_TOML)
            .map(|(_, theme)| theme)
            .unwrap_or_else(|_| AppTheme::dark())
    }

    /// 从内置浅色 TOML 构造主题，供旧 builtin 模块保持兼容。
    pub fn builtin_light_theme() -> AppTheme {
        parse_theme_toml(BUILTIN_LIGHT_THEME_TOML)
            .map(|(_, theme)| theme)
            .unwrap_or_else(|_| AppTheme::light())
    }

    /// 加载内置主题；这里是正常主题路径，只有 TOML 被破坏时才进入硬编码紧急 fallback。
    fn load_builtin_themes() -> HashMap<String, AppTheme> {
        let mut themes = HashMap::new();
        let dark = Self::builtin_dark_theme();
        let light = Self::builtin_light_theme();
        themes.insert(DARK_MODE.to_string(), dark);
        themes.insert(LIGHT_MODE.to_string(), light);
        themes
    }

    /// 扫描用户主题目录，非法文件只记录 warning，不影响应用启动。
    fn load_user_themes(&mut self, user_theme_dir: &Path) {
        if !user_theme_dir.exists() {
            return;
        }

        let entries = match fs::read_dir(user_theme_dir) {
            Ok(entries) => entries,
            Err(error) => {
                self.warnings.push(format!(
                    "无法读取用户主题目录 {}：{error}",
                    user_theme_dir.display()
                ));
                return;
            }
        };

        for entry_result in entries {
            let entry = match entry_result {
                Ok(entry) => entry,
                Err(error) => {
                    self.warnings
                        .push(format!("读取用户主题目录条目失败：{error}"));
                    continue;
                }
            };
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
                continue;
            }

            match fs::read_to_string(&path)
                .map_err(|error| error.to_string())
                .and_then(|text| parse_theme_toml(&text).map_err(|error| error.to_string()))
            {
                Ok((file, theme)) => self.user_themes.push(LoadedUserTheme {
                    name: file.name,
                    mode: file.mode,
                    path,
                    theme,
                }),
                Err(error) => self
                    .warnings
                    .push(format!("跳过用户主题 {}：{error}", path.display())),
            }
        }
    }
}

impl Default for ThemeManager {
    /// 构造默认主题管理器。
    fn default() -> Self {
        Self::load_default()
    }
}

/// 解析单个主题 TOML 文本。
///
/// 返回值：主题文件元信息和运行期主题令牌。
fn parse_theme_toml(text: &str) -> Result<(ThemeFile, AppTheme), ThemeError> {
    let file = toml::from_str::<ThemeFile>(text)?;
    let Some(canonical_mode) = canonical_mode(&file.mode) else {
        return Err(ThemeError::UnsupportedMode {
            name: file.name,
            mode: file.mode,
        });
    };
    let syntax_fallback = if canonical_mode == LIGHT_MODE {
        SyntaxTheme::light()
    } else {
        SyntaxTheme::dark()
    };
    let syntax = parse_syntax_theme(file.syntax.as_ref(), syntax_fallback)?;

    let theme = AppTheme {
        background: parse_hex_color("colors.background", &file.colors.background)?,
        title_bar: parse_hex_color("colors.title_bar", &file.colors.title_bar)?,
        activity_bar: parse_hex_color("colors.activity_bar", &file.colors.activity_bar)?,
        side_bar: parse_hex_color("colors.side_bar", &file.colors.side_bar)?,
        content: parse_hex_color("colors.content", &file.colors.content)?,
        status_bar: parse_hex_color("colors.status_bar", &file.colors.status_bar)?,
        foreground: parse_hex_color("colors.foreground", &file.colors.foreground)?,
        foreground_muted: parse_hex_color(
            "colors.foreground_muted",
            &file.colors.foreground_muted,
        )?,
        border: parse_hex_color("colors.border", &file.colors.border)?,
        selection: parse_hex_color("colors.selection", &file.colors.selection)?,
        current_line: parse_hex_color("colors.current_line", &file.colors.current_line)?,
        debug: parse_hex_color("log_levels.debug", &file.log_levels.debug)?,
        info: parse_hex_color("log_levels.info", &file.log_levels.info)?,
        warning: parse_hex_color("log_levels.warning", &file.log_levels.warning)?,
        error: parse_hex_color("log_levels.error", &file.log_levels.error)?,
        success: parse_hex_color("log_levels.success", &file.log_levels.success)?,
        modal_overlay: parse_hex_color("colors.modal_overlay", &file.colors.modal_overlay)?,
        syntax,
    };

    Ok((file, theme))
}

/// 解析 `#RRGGBB` 或 `#RRGGBBAA` 颜色值为 GPUI 当前使用的整数颜色。
fn parse_hex_color(token: &'static str, value: &str) -> Result<u32, ThemeError> {
    let hex = value
        .trim()
        .strip_prefix('#')
        .ok_or_else(|| ThemeError::InvalidColor {
            token,
            value: value.to_string(),
        })?;

    if hex.len() != 6 && hex.len() != 8 {
        return Err(ThemeError::InvalidColor {
            token,
            value: value.to_string(),
        });
    }

    u32::from_str_radix(hex, 16).map_err(|_| ThemeError::InvalidColor {
        token,
        value: value.to_string(),
    })
}

/// 解析可选语法高亮颜色；缺失字段沿用当前主题模式默认值。
fn parse_syntax_theme(
    tokens: Option<&ThemeSyntaxTokens>,
    mut fallback: SyntaxTheme,
) -> Result<SyntaxTheme, ThemeError> {
    let Some(tokens) = tokens else {
        return Ok(fallback);
    };

    apply_optional_color("syntax.comment", &tokens.comment, &mut fallback.comment)?;
    apply_optional_color("syntax.key", &tokens.key, &mut fallback.key)?;
    apply_optional_color("syntax.string", &tokens.string, &mut fallback.string)?;
    apply_optional_color("syntax.number", &tokens.number, &mut fallback.number)?;
    apply_optional_color("syntax.boolean", &tokens.boolean, &mut fallback.boolean)?;
    apply_optional_color(
        "syntax.punctuation",
        &tokens.punctuation,
        &mut fallback.punctuation,
    )?;
    apply_optional_color("syntax.tag", &tokens.tag, &mut fallback.tag)?;
    apply_optional_color(
        "syntax.attribute",
        &tokens.attribute,
        &mut fallback.attribute,
    )?;
    apply_optional_color(
        "syntax.timestamp",
        &tokens.timestamp,
        &mut fallback.timestamp,
    )?;
    apply_optional_color("syntax.thread", &tokens.thread, &mut fallback.thread)?;
    apply_optional_color("syntax.class", &tokens.class, &mut fallback.class)?;
    apply_optional_color("syntax.method", &tokens.method, &mut fallback.method)?;
    apply_optional_color("syntax.lock", &tokens.lock, &mut fallback.lock)?;
    apply_optional_color(
        "syntax.exception",
        &tokens.exception,
        &mut fallback.exception,
    )?;

    Ok(fallback)
}

/// 将可选颜色字段覆盖到目标 token 上。
fn apply_optional_color(
    token: &'static str,
    value: &Option<String>,
    target: &mut u32,
) -> Result<(), ThemeError> {
    if let Some(value) = value {
        *target = parse_hex_color(token, value)?;
    }
    Ok(())
}

/// 规范化主题模式，避免配置文件大小写差异导致读取失败。
fn normalize_mode(mode: &str) -> &str {
    canonical_mode(mode).unwrap_or(DARK_MODE)
}

/// 尝试规范化主题模式，不认识的值返回 None 交给调用方决定是否报错。
fn canonical_mode(mode: &str) -> Option<&'static str> {
    match mode.trim().to_ascii_lowercase().as_str() {
        "light" => Some(LIGHT_MODE),
        "dark" => Some(DARK_MODE),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证内置深色 TOML 解析结果等价于当前代码中的深色 fallback。
    #[test]
    fn builtin_dark_toml_matches_current_dark_colors() {
        let (_, theme) = parse_theme_toml(BUILTIN_DARK_THEME_TOML).expect("内置深色主题应可解析");

        assert_eq!(theme.background, AppTheme::dark().background);
        assert_eq!(theme.title_bar, AppTheme::dark().title_bar);
        assert_eq!(theme.modal_overlay, AppTheme::dark().modal_overlay);
        assert_eq!(theme.syntax.comment, AppTheme::dark().syntax.comment);
    }

    /// 验证内置浅色 TOML 解析结果等价于当前代码中的浅色 fallback。
    #[test]
    fn builtin_light_toml_matches_current_light_colors() {
        let (_, theme) = parse_theme_toml(BUILTIN_LIGHT_THEME_TOML).expect("内置浅色主题应可解析");

        assert_eq!(theme.content, AppTheme::light().content);
        assert_eq!(theme.selection, AppTheme::light().selection);
        assert_eq!(theme.success, AppTheme::light().success);
        assert_eq!(theme.syntax.string, AppTheme::light().syntax.string);
    }

    /// 验证 6 位和 8 位十六进制颜色都能解析。
    #[test]
    fn parse_hex_color_supports_rgb_and_rgba() {
        assert_eq!(parse_hex_color("test", "#112233").unwrap(), 0x112233);
        assert_eq!(parse_hex_color("test", "#112233aa").unwrap(), 0x112233aa);
    }

    /// 验证非法颜色会返回明确错误，便于主题校验提示。
    #[test]
    fn parse_hex_color_rejects_invalid_value() {
        let error = parse_hex_color("colors.content", "112233").expect_err("缺少 # 应报错");

        assert!(matches!(error, ThemeError::InvalidColor { .. }));
    }

    /// 验证用户旧主题缺少 syntax 段时仍能加载，并自动使用对应模式默认语法颜色。
    #[test]
    fn missing_syntax_tokens_use_mode_fallback() {
        let text = r##"
name = "Legacy Light"
mode = "light"

[colors]
background = "#f3f3f3"
title_bar = "#e8e8e8"
activity_bar = "#e6e6e6"
side_bar = "#eeeeee"
content = "#ffffff"
status_bar = "#f2f2f2"
foreground = "#242424"
foreground_muted = "#6b6b6b"
border = "#d0d0d0"
selection = "#d7e8fb"
current_line = "#ebf2f8"
modal_overlay = "#e9edf3c2"

[log_levels]
debug = "#4f7a37"
info = "#0969da"
warning = "#9a6700"
error = "#cf222e"
success = "#1a7f37"
"##;
        let (_, theme) = parse_theme_toml(text).expect("旧主题缺少 syntax 段也应可解析");

        assert_eq!(theme.syntax.comment, SyntaxTheme::light().comment);
        assert_eq!(theme.syntax.exception, SyntaxTheme::light().exception);
    }

    /// 验证非法用户主题不会阻塞主题管理器创建。
    #[test]
    fn invalid_user_theme_is_skipped_with_warning() {
        let dir =
            std::env::temp_dir().join(format!("argus-theme-test-{}-invalid", std::process::id()));
        fs::create_dir_all(&dir).expect("测试主题目录应可创建");
        fs::write(dir.join("broken.toml"), "name = [").expect("测试坏主题应可写入");

        let manager = ThemeManager::load_with_user_theme_dir(&dir);

        assert!(manager.user_themes().is_empty());
        assert!(!manager.warnings().is_empty());
    }
}
