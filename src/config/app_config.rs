//! 文件职责：定义应用运行期配置与持久化设置模型。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-25
//! 作者：Argus 开发团队
//! 主要功能：提供外观、日志加载、日志搜索、链接、编码、缓存和升级设置的默认值、校验和 TOML 序列化结构。

use crate::connections::ConnectionConfig;
use serde::{Deserialize, Serialize};

/// 默认 Jstack 线程名过滤规则，隐藏常见编译线程和 JVM 附加监听线程。
pub const DEFAULT_JSTACK_THREAD_NAME_FILTERS: &str =
    "C1 CompilerThread*,C2 CompilerThread*,Attach Listener";
/// 默认 Jstack 完整线程段过滤规则，隐藏常见 Resin keepalive socket 读取和 accept 等待堆栈。
pub const DEFAULT_JSTACK_STACK_SEGMENT_FILTERS: &str = concat!(
    "java.lang.Thread.State: RUNNABLE\n",
    "\tat java.net.SocketInputStream.socketRead0(Native Method)\n",
    "\tat java.net.SocketInputStream.socketRead(SocketInputStream.java:116)\n",
    "\tat java.net.SocketInputStream.read(SocketInputStream.java:171)\n",
    "\tat java.net.SocketInputStream.read(SocketInputStream.java:141)\n",
    "\tat sun.security.ssl.InputRecord.readFully(InputRecord.java:465)\n",
    "\tat sun.security.ssl.InputRecord.read(InputRecord.java:503)\n",
    "\tat sun.security.ssl.SSLSocketImpl.readRecord(SSLSocketImpl.java:983)\n",
    "\t- locked <0x000000069ea415d8> (a java.lang.Object)\n",
    "\tat sun.security.ssl.SSLSocketImpl.readDataRecord(SSLSocketImpl.java:940)\n",
    "\tat sun.security.ssl.AppInputStream.read(AppInputStream.java:105)\n",
    "\t- locked <0x000000069ea41620> (a sun.security.ssl.AppInputStream)\n",
    "\tat com.caucho.vfs.SocketStream.read(SocketStream.java:187)\n",
    "\tat com.caucho.vfs.SocketStream.readTimeout(SocketStream.java:239)\n",
    "\tat com.caucho.vfs.ReadStream.fillWithTimeout(ReadStream.java:1147)\n",
    "\tat com.caucho.network.listen.TcpSocketLink.threadKeepalive(TcpSocketLink.java:1482)\n",
    "\tat com.caucho.network.listen.TcpSocketLink.processKeepalive(TcpSocketLink.java:1460)\n",
    "\tat com.caucho.network.listen.TcpSocketLink.handleRequestsImpl(TcpSocketLink.java:1300)\n",
    "\tat com.caucho.network.listen.TcpSocketLink.handleRequests(TcpSocketLink.java:1215)\n",
    "\tat com.caucho.network.listen.TcpSocketLink.handleAcceptTaskImpl(TcpSocketLink.java:1011)\n",
    "\tat com.caucho.network.listen.ConnectionTask.runThread(ConnectionTask.java:117)\n",
    "\tat com.caucho.network.listen.ConnectionTask.run(ConnectionTask.java:93)\n",
    "\tat com.caucho.network.listen.SocketLinkThreadLauncher.handleTasks(SocketLinkThreadLauncher.java:175)\n",
    "\tat com.caucho.network.listen.TcpSocketAcceptThread.run(TcpSocketAcceptThread.java:61)\n",
    "\tat com.caucho.env.thread2.ResinThread2.runTasks(ResinThread2.java:173)\n",
    "\tat com.caucho.env.thread2.ResinThread2.run(ResinThread2.java:118)\n",
    "\n\n",
    "java.lang.Thread.State: RUNNABLE\n",
    "\tat java.net.DualStackPlainSocketImpl.accept0(Native Method)\n",
    "\tat java.net.DualStackPlainSocketImpl.socketAccept(DualStackPlainSocketImpl.java:131)\n",
    "\tat java.net.AbstractPlainSocketImpl.accept(AbstractPlainSocketImpl.java:409)\n",
    "\tat java.net.PlainSocketImpl.accept(PlainSocketImpl.java:199)\n",
    "\t- locked <0x000000061ff12688> (a java.net.SocksSocketImpl)\n",
    "\tat java.net.ServerSocket.implAccept(ServerSocket.java:545)\n",
    "\tat sun.security.ssl.SSLServerSocketImpl.accept(SSLServerSocketImpl.java:348)\n",
    "\tat com.caucho.vfs.QServerSocketWrapper.accept(QServerSocketWrapper.java:105)\n",
    "\tat com.caucho.network.listen.TcpPort.accept(TcpPort.java:1380)\n",
    "\tat com.caucho.network.listen.TcpSocketLink.accept(TcpSocketLink.java:1039)\n",
    "\tat com.caucho.network.listen.TcpSocketLink.handleAcceptTaskImpl(TcpSocketLink.java:989)\n",
    "\tat com.caucho.network.listen.ConnectionTask.runThread(ConnectionTask.java:117)\n",
    "\tat com.caucho.network.listen.ConnectionTask.run(ConnectionTask.java:93)\n",
    "\tat com.caucho.network.listen.SocketLinkThreadLauncher.handleTasks(SocketLinkThreadLauncher.java:175)\n",
    "\tat com.caucho.network.listen.TcpSocketAcceptThread.run(TcpSocketAcceptThread.java:61)\n",
    "\tat com.caucho.env.thread2.ResinThread2.runTasks(ResinThread2.java:173)\n",
    "\tat com.caucho.env.thread2.ResinThread2.run(ResinThread2.java:118)",
);

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
    /// 日志显示配置，保存阅读区和线程分析展示偏好。
    #[serde(default)]
    pub log_display: LogDisplayConfig,
    /// 链接工作区配置，保存目录树、SSH 链接和受信主机指纹。
    #[serde(default)]
    pub connections: ConnectionConfig,
    /// 编码配置，后续日志读取模块会据此选择默认解码策略。
    #[serde(default)]
    pub encoding: EncodingConfig,
    /// 缓存配置，后续索引和读取模块会据此控制临时缓存策略。
    #[serde(default)]
    pub cache: CacheConfig,
    /// 升级配置，控制是否从用户配置的服务器检查并安装新版本。
    #[serde(default)]
    pub upgrade: UpgradeConfig,
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
        self.log_display.jstack_thread_name_filters =
            normalized_inline_text(self.log_display.jstack_thread_name_filters);
        self.log_display.jstack_stack_segment_filters =
            normalized_stack_segment_filter_text(self.log_display.jstack_stack_segment_filters);
        self.connections = self.connections.normalized();
        self.cache.limit_mb = self.cache.limit_mb.clamp(128, 2048);
        if self.encoding.selected.trim().is_empty() {
            self.encoding.selected = EncodingConfig::default().selected;
        }
        self.upgrade.server_url = self.upgrade.server_url.trim().to_string();
        self.upgrade.public_key_base64 = self.upgrade.public_key_base64.trim().to_string();
        self.upgrade.skipped_version = normalized_optional_text(self.upgrade.skipped_version);
        self.upgrade.last_check_at = normalized_optional_text(self.upgrade.last_check_at);
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
            log_display: LogDisplayConfig::default(),
            connections: ConnectionConfig::default(),
            encoding: EncodingConfig::default(),
            cache: CacheConfig::default(),
            upgrade: UpgradeConfig::default(),
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

/// 日志显示配置，保存阅读区和线程日志分析的展示偏好。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LogDisplayConfig {
    /// Jstack 线程名过滤关键字，多个关键字可用逗号、分号或竖线分隔。
    #[serde(default = "default_jstack_thread_name_filters")]
    pub jstack_thread_name_filters: String,
    /// Jstack 完整线程段过滤片段，多个片段使用空行分隔，`\n` 会按换行匹配。
    #[serde(default = "default_jstack_stack_segment_filters")]
    pub jstack_stack_segment_filters: String,
}

impl Default for LogDisplayConfig {
    /// 构造默认日志显示配置，默认过滤常见低价值 Jstack 系统线程和网络等待堆栈。
    fn default() -> Self {
        Self {
            jstack_thread_name_filters: default_jstack_thread_name_filters(),
            jstack_stack_segment_filters: default_jstack_stack_segment_filters(),
        }
    }
}

/// 返回默认 Jstack 线程名过滤规则，供 serde 缺失字段和默认配置复用。
fn default_jstack_thread_name_filters() -> String {
    DEFAULT_JSTACK_THREAD_NAME_FILTERS.to_string()
}

/// 返回默认 Jstack 线程段过滤规则，供 serde 缺失字段和默认配置复用。
fn default_jstack_stack_segment_filters() -> String {
    DEFAULT_JSTACK_STACK_SEGMENT_FILTERS.to_string()
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

/// 自动升级配置，保存升级服务器和用户跳过版本等跨会话偏好。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UpgradeConfig {
    /// 是否启用启动时自动检查升级。
    pub enabled: bool,
    /// 升级服务器基础地址；为空时不会发起网络请求。
    pub server_url: String,
    /// Ed25519 验签公钥 Base64；为空时不会信任任何升级清单。
    #[serde(default)]
    pub public_key_base64: String,
    /// 用户选择跳过的版本号，自动检查时不再弹出该版本。
    #[serde(default)]
    pub skipped_version: Option<String>,
    /// 最近一次检查升级的 RFC3339 时间戳，仅用于设置页展示和诊断。
    #[serde(default)]
    pub last_check_at: Option<String>,
}

impl Default for UpgradeConfig {
    /// 构造默认升级配置，避免新安装用户在没有服务器地址时产生网络访问。
    fn default() -> Self {
        Self {
            enabled: false,
            server_url: String::new(),
            public_key_base64: String::new(),
            skipped_version: None,
            last_check_at: None,
        }
    }
}

/// 归一化可选文本配置，去掉空白并把空字符串折叠成 `None`。
fn normalized_optional_text(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// 归一化单行设置文本，清理首尾空白并把回车换行折叠为空格。
fn normalized_inline_text(value: String) -> String {
    value.replace(['\r', '\n'], " ").trim().to_string()
}

/// 归一化多行设置文本，统一换行符并清理首尾空白，保留用户粘贴的堆栈结构。
fn normalized_multiline_text(value: String) -> String {
    value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim()
        .to_string()
}

/// 归一化 Jstack 线程段过滤配置，并把旧版 `||` 分隔迁移为空行分隔。
fn normalized_stack_segment_filter_text(value: String) -> String {
    normalized_multiline_text(value).replace("||", "\n\n")
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
            log_display: LogDisplayConfig {
                jstack_thread_name_filters: " main, Attach Listener ".to_string(),
                jstack_stack_segment_filters: " java.net.SocketInputStream||read ".to_string(),
            },
            connections: ConnectionConfig::default(),
            encoding: EncodingConfig {
                selected: String::new(),
            },
            cache: CacheConfig {
                enabled: true,
                limit_mb: 1,
            },
            upgrade: UpgradeConfig {
                enabled: true,
                server_url: " https://updates.example.com/argus ".to_string(),
                public_key_base64: " TEST_PUBLIC_KEY_BASE64 ".to_string(),
                skipped_version: Some(" 0.2.0 ".to_string()),
                last_check_at: Some(" ".to_string()),
            },
        }
        .normalized();

        assert_eq!(config.appearance.log_content_font_size, 20.0);
        assert_eq!(config.appearance.theme_mode, "dark.toml");
        assert_eq!(config.loader.max_archive_depth, 8);
        assert_eq!(config.loader.archive_probe_concurrency, 16);
        assert_eq!(config.log_search.quick_keywords, "ERROR, WARN");
        assert_eq!(
            config.log_display.jstack_thread_name_filters,
            "main, Attach Listener"
        );
        assert_eq!(
            config.log_display.jstack_stack_segment_filters,
            "java.net.SocketInputStream\n\nread"
        );
        assert_eq!(config.encoding.selected, "UTF-8");
        assert_eq!(config.cache.limit_mb, 128);
        assert_eq!(
            config.upgrade.server_url,
            "https://updates.example.com/argus"
        );
        assert_eq!(config.upgrade.public_key_base64, "TEST_PUBLIC_KEY_BASE64");
        assert_eq!(config.upgrade.skipped_version.as_deref(), Some("0.2.0"));
        assert_eq!(config.upgrade.last_check_at, None);
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

    /// 验证日志显示配置默认隐藏常见低价值 Jstack 线程和网络等待堆栈。
    #[test]
    fn default_log_display_filters_use_jstack_noise_patterns() {
        let config = LogDisplayConfig::default();

        assert_eq!(
            config.jstack_thread_name_filters,
            DEFAULT_JSTACK_THREAD_NAME_FILTERS
        );
        assert!(
            config
                .jstack_thread_name_filters
                .contains("C1 CompilerThread*")
        );
        assert!(
            config
                .jstack_stack_segment_filters
                .contains("SocketInputStream.socketRead0")
        );
        assert!(
            config
                .jstack_stack_segment_filters
                .contains("DualStackPlainSocketImpl.accept0")
        );
        assert!(!config.jstack_stack_segment_filters.contains("||"));
        assert!(config.jstack_stack_segment_filters.contains("\n\n"));
    }

    /// 验证默认升级配置不会在未配置服务器时发起自动检查。
    #[test]
    fn default_upgrade_config_is_disabled() {
        let config = UpgradeConfig::default();

        assert!(!config.enabled);
        assert!(config.server_url.is_empty());
        assert!(config.public_key_base64.is_empty());
        assert!(config.skipped_version.is_none());
        assert!(config.last_check_at.is_none());
    }
}
