//! 文件职责：定义应用运行期配置与持久化设置模型。
//! 创建日期：2026-06-09
//! 修改日期：2026-09-24
//! 作者：Argus 开发团队
//! 主要功能：提供 AI、外观、日志加载、日志搜索、链接和编码设置的默认值及 TOML 模型。

use crate::config::ai_config::AiConfig;
use crate::remote::connection::ConnectionConfig;
use serde::{Deserialize, Serialize};

/// 默认 Jstack 线程段过滤规则一：Resin keepalive socket 读取等待堆栈。
/// 锁地址已写成 `<0x>` 归一化形式、行首不带缩进，与匹配侧归一化保持一致。
const DEFAULT_JSTACK_STACK_SEGMENT_FILTER_KEEPALIVE: &str = concat!(
    "java.lang.Thread.State: RUNNABLE\n",
    "at java.net.SocketInputStream.socketRead0(Native Method)\n",
    "at java.net.SocketInputStream.socketRead(SocketInputStream.java:116)\n",
    "at java.net.SocketInputStream.read(SocketInputStream.java:171)\n",
    "at java.net.SocketInputStream.read(SocketInputStream.java:141)\n",
    "at sun.security.ssl.InputRecord.readFully(InputRecord.java:465)\n",
    "at sun.security.ssl.InputRecord.read(InputRecord.java:503)\n",
    "at sun.security.ssl.SSLSocketImpl.readRecord(SSLSocketImpl.java:983)\n",
    "- locked <0x> (a java.lang.Object)\n",
    "at sun.security.ssl.SSLSocketImpl.readDataRecord(SSLSocketImpl.java:940)\n",
    "at sun.security.ssl.AppInputStream.read(AppInputStream.java:105)\n",
    "- locked <0x> (a sun.security.ssl.AppInputStream)\n",
    "at com.caucho.vfs.SocketStream.read(SocketStream.java:187)\n",
    "at com.caucho.vfs.SocketStream.readTimeout(SocketStream.java:239)\n",
    "at com.caucho.vfs.ReadStream.fillWithTimeout(ReadStream.java:1147)\n",
    "at com.caucho.network.listen.TcpSocketLink.threadKeepalive(TcpSocketLink.java:1482)\n",
    "at com.caucho.network.listen.TcpSocketLink.processKeepalive(TcpSocketLink.java:1460)\n",
    "at com.caucho.network.listen.TcpSocketLink.handleRequestsImpl(TcpSocketLink.java:1300)\n",
    "at com.caucho.network.listen.TcpSocketLink.handleRequests(TcpSocketLink.java:1215)\n",
    "at com.caucho.network.listen.TcpSocketLink.handleAcceptTaskImpl(TcpSocketLink.java:1011)\n",
    "at com.caucho.network.listen.ConnectionTask.runThread(ConnectionTask.java:117)\n",
    "at com.caucho.network.listen.ConnectionTask.run(ConnectionTask.java:93)\n",
    "at com.caucho.network.listen.SocketLinkThreadLauncher.handleTasks(SocketLinkThreadLauncher.java:175)\n",
    "at com.caucho.network.listen.TcpSocketAcceptThread.run(TcpSocketAcceptThread.java:61)\n",
    "at com.caucho.env.thread2.ResinThread2.runTasks(ResinThread2.java:173)\n",
    "at com.caucho.env.thread2.ResinThread2.run(ResinThread2.java:118)",
);
/// 默认 Jstack 线程段过滤规则二：Resin accept 等待堆栈。
/// 锁地址已写成 `<0x>` 归一化形式、行首不带缩进，与匹配侧归一化保持一致。
const DEFAULT_JSTACK_STACK_SEGMENT_FILTER_ACCEPT: &str = concat!(
    "java.lang.Thread.State: RUNNABLE\n",
    "at java.net.DualStackPlainSocketImpl.accept0(Native Method)\n",
    "at java.net.DualStackPlainSocketImpl.socketAccept(DualStackPlainSocketImpl.java:131)\n",
    "at java.net.AbstractPlainSocketImpl.accept(AbstractPlainSocketImpl.java:409)\n",
    "at java.net.PlainSocketImpl.accept(PlainSocketImpl.java:199)\n",
    "- locked <0x> (a java.net.SocksSocketImpl)\n",
    "at java.net.ServerSocket.implAccept(ServerSocket.java:545)\n",
    "at sun.security.ssl.SSLServerSocketImpl.accept(SSLServerSocketImpl.java:348)\n",
    "at com.caucho.vfs.QServerSocketWrapper.accept(QServerSocketWrapper.java:105)\n",
    "at com.caucho.network.listen.TcpPort.accept(TcpPort.java:1380)\n",
    "at com.caucho.network.listen.TcpSocketLink.accept(TcpSocketLink.java:1039)\n",
    "at com.caucho.network.listen.TcpSocketLink.handleAcceptTaskImpl(TcpSocketLink.java:989)\n",
    "at com.caucho.network.listen.ConnectionTask.runThread(ConnectionTask.java:117)\n",
    "at com.caucho.network.listen.ConnectionTask.run(ConnectionTask.java:93)\n",
    "at com.caucho.network.listen.SocketLinkThreadLauncher.handleTasks(SocketLinkThreadLauncher.java:175)\n",
    "at com.caucho.network.listen.TcpSocketAcceptThread.run(TcpSocketAcceptThread.java:61)\n",
    "at com.caucho.env.thread2.ResinThread2.runTasks(ResinThread2.java:173)\n",
    "at com.caucho.env.thread2.ResinThread2.run(ResinThread2.java:118)",
);

/// 应用配置根对象，字段结构与 `~/.argus/settings.toml` 保持一致。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct AppConfig {
    /// AI 日志分析配置；API Key 不包含在该结构中。
    #[serde(default)]
    pub ai: AiConfig,
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
}

impl AppConfig {
    /// 返回经过边界修正的配置副本。
    ///
    /// 返回值：所有数值型配置均被限制在当前 UI 可展示范围内，避免坏配置破坏界面状态。
    pub(crate) fn normalized(mut self) -> Self {
        self.ai.normalize();
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
        self.log_search.quick_keywords = self.log_search.quick_keywords.trim().to_string();
        self.log_search
            .recent_keywords
            .truncate(SEARCH_RECENT_KEYWORDS_MAX);
        self.log_display.normalize_jstack_thread_filter_rules();
        self.connections = self.connections.normalized();
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
            ai: AiConfig::default(),
            appearance: AppearanceConfig::default(),
            loader: LoaderConfig::default(),
            log_search: LogSearchConfig::default(),
            log_display: LogDisplayConfig::default(),
            connections: ConnectionConfig::default(),
            encoding: EncodingConfig::default(),
        }
    }
}

/// 外观配置，持久化设置页中的主题文件和日志内容字号。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct AppearanceConfig {
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
pub(crate) struct LoaderConfig {
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

/// 搜索关键字历史最多保留条数；超出时丢弃最旧项。
pub(crate) const SEARCH_RECENT_KEYWORDS_MAX: usize = 20;

/// 日志搜索配置，保存快搜关键字与最近搜索历史。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct LogSearchConfig {
    /// 快搜关键字原始输入，使用英文逗号分隔；解析和去重在搜索启动时执行。
    pub quick_keywords: String,
    /// 最近搜索关键字历史，最新在前；保存最近 20 条，搜索对话框关键字输入框下拉展示。
    #[serde(default)]
    pub recent_keywords: Vec<String>,
}

/// Jstack 线程过滤规则：按线程名关键字或完整线程段片段隐藏分析行。
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub(crate) struct JstackThreadFilterRule {
    /// 规则是否启用；停用保留配置但不参与匹配。
    #[serde(default = "default_jstack_thread_filter_rule_enabled")]
    pub enabled: bool,
    /// 匹配方式：线程名关键字（支持 * ? 通配）或完整线程段片段。
    #[serde(default)]
    pub kind: JstackThreadFilterRuleKind,
    /// 匹配内容；线程段片段可包含多行堆栈文本。
    #[serde(default)]
    pub pattern: String,
}

/// Jstack 线程过滤规则匹配方式。
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum JstackThreadFilterRuleKind {
    /// 线程名关键字或通配符（默认）。
    #[default]
    ThreadName,
    /// 完整线程段片段，按子串匹配。
    StackSegment,
}

/// 日志显示配置，保存阅读区和线程日志分析的展示偏好。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct LogDisplayConfig {
    /// Jstack 线程过滤规则列表；旧版自由文本配置在加载迁移后由该列表承载。
    #[serde(default)]
    pub jstack_thread_filter_rules: Vec<JstackThreadFilterRule>,
    /// 旧版 Jstack 线程名过滤文本（逗号、分号、竖线或换行分隔），仅用于读取旧设置文件并一次性迁移。
    #[serde(default, skip_serializing)]
    pub jstack_thread_name_filters: String,
    /// 旧版 Jstack 线程段过滤文本（空行分隔），仅用于读取旧设置文件并一次性迁移。
    #[serde(default, skip_serializing)]
    pub jstack_stack_segment_filters: String,
}

impl LogDisplayConfig {
    /// 把旧版自由文本过滤配置一次性迁移为规则列表；已使用规则列表的配置仅清理残留旧字段。
    ///
    /// 说明：旧字段序列化时不再写出，迁移只发生在加载旧设置文件的归一化阶段。
    fn normalize_jstack_thread_filter_rules(&mut self) {
        if self.jstack_thread_filter_rules.is_empty() {
            let mut migrated_rules = Vec::new();
            for pattern in self
                .jstack_thread_name_filters
                .split([',', ';', '|', '\n', '\r', '，', '；'])
            {
                let pattern = pattern.trim();
                if !pattern.is_empty() {
                    migrated_rules.push(JstackThreadFilterRule {
                        enabled: true,
                        kind: JstackThreadFilterRuleKind::ThreadName,
                        pattern: pattern.to_string(),
                    });
                }
            }
            for block in crate::analysis::jstack::legacy_stack_segment_filter_blocks(
                &self.jstack_stack_segment_filters,
            ) {
                let block = block.trim();
                if !block.is_empty() {
                    migrated_rules.push(JstackThreadFilterRule {
                        enabled: true,
                        kind: JstackThreadFilterRuleKind::StackSegment,
                        pattern: block.to_string(),
                    });
                }
            }
            self.jstack_thread_filter_rules = migrated_rules;
        }
        self.jstack_thread_name_filters.clear();
        self.jstack_stack_segment_filters.clear();
    }
}

impl Default for LogDisplayConfig {
    /// 构造默认日志显示配置，默认过滤常见低价值 Jstack 系统线程和网络等待堆栈。
    fn default() -> Self {
        Self {
            jstack_thread_filter_rules: default_jstack_thread_filter_rules(),
            jstack_thread_name_filters: String::new(),
            jstack_stack_segment_filters: String::new(),
        }
    }
}

/// 返回规则是否启用的 serde 默认值。
fn default_jstack_thread_filter_rule_enabled() -> bool {
    true
}

/// 返回默认 Jstack 线程过滤规则，供默认配置复用。
///
/// 说明：故意不作为 `jstack_thread_filter_rules` 的 serde 默认值，否则旧设置文件加载时
/// 规则列表先被默认值填满，旧字段迁移将永远无法触发；缺失该字段的旧文件迁移后
/// 得到空规则列表，与新用户显式清空规则的行为保持一致。
fn default_jstack_thread_filter_rules() -> Vec<JstackThreadFilterRule> {
    let thread_name_rule = |pattern: &str| JstackThreadFilterRule {
        enabled: true,
        kind: JstackThreadFilterRuleKind::ThreadName,
        pattern: pattern.to_string(),
    };
    let stack_segment_rule = |pattern: &str| JstackThreadFilterRule {
        enabled: true,
        kind: JstackThreadFilterRuleKind::StackSegment,
        pattern: pattern.to_string(),
    };

    vec![
        thread_name_rule("C1 CompilerThread*"),
        thread_name_rule("C2 CompilerThread*"),
        thread_name_rule("Attach Listener"),
        stack_segment_rule(DEFAULT_JSTACK_STACK_SEGMENT_FILTER_KEEPALIVE),
        stack_segment_rule(DEFAULT_JSTACK_STACK_SEGMENT_FILTER_ACCEPT),
    ]
}

/// 编码配置，当前先持久化用户选择，日志正文读取接入后再参与解码。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct EncodingConfig {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证坏配置会被修正到 UI 允许的范围内。
    #[test]
    fn normalized_clamps_numeric_settings() {
        let config = AppConfig {
            ai: AiConfig::default(),
            appearance: AppearanceConfig {
                theme_mode: "light".to_string(),
                log_content_font_size: 99.0,
            },
            loader: LoaderConfig {
                max_archive_depth: 99,
                follow_symlinks: true,
            },
            log_search: LogSearchConfig {
                quick_keywords: " ERROR, WARN ".to_string(),
                recent_keywords: Vec::new(),
            },
            log_display: LogDisplayConfig {
                jstack_thread_filter_rules: Vec::new(),
                jstack_thread_name_filters: " main, Attach Listener ".to_string(),
                jstack_stack_segment_filters: " java.net.SocketInputStream||read ".to_string(),
            },
            connections: ConnectionConfig::default(),
            encoding: EncodingConfig {
                selected: String::new(),
            },
        }
        .normalized();

        assert_eq!(config.appearance.log_content_font_size, 20.0);
        assert_eq!(config.appearance.theme_mode, "dark.toml");
        assert_eq!(config.loader.max_archive_depth, 8);
        assert_eq!(config.log_search.quick_keywords, "ERROR, WARN");
        let rules = &config.log_display.jstack_thread_filter_rules;
        assert_eq!(rules.len(), 4);
        assert!(
            rules
                .iter()
                .take(2)
                .all(|rule| rule.enabled && rule.kind == JstackThreadFilterRuleKind::ThreadName)
        );
        assert_eq!(rules[0].pattern, "main");
        assert_eq!(rules[1].pattern, "Attach Listener");
        assert_eq!(rules[2].kind, JstackThreadFilterRuleKind::StackSegment);
        assert_eq!(rules[2].pattern, "java.net.SocketInputStream");
        assert_eq!(rules[3].pattern, "read");
        assert!(config.log_display.jstack_thread_name_filters.is_empty());
        assert!(config.log_display.jstack_stack_segment_filters.is_empty());
        assert_eq!(config.encoding.selected, "UTF-8");
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
        let rules = &config.jstack_thread_filter_rules;

        assert_eq!(rules.len(), 5);
        assert!(rules.iter().all(|rule| rule.enabled));
        assert_eq!(rules[0].kind, JstackThreadFilterRuleKind::ThreadName);
        assert_eq!(rules[0].pattern, "C1 CompilerThread*");
        assert_eq!(rules[1].pattern, "C2 CompilerThread*");
        assert_eq!(rules[2].pattern, "Attach Listener");
        assert_eq!(rules[3].kind, JstackThreadFilterRuleKind::StackSegment);
        assert_eq!(rules[4].kind, JstackThreadFilterRuleKind::StackSegment);
        assert!(rules[3].pattern.contains("SocketInputStream.socketRead0"));
        assert!(
            rules[4]
                .pattern
                .contains("DualStackPlainSocketImpl.accept0")
        );
        // 默认线程段规则必须使用 `<0x>` 归一化锁地址且行首不带缩进，保证能匹配真实 dump。
        for rule in &rules[3..] {
            assert!(rule.pattern.contains("<0x>"));
            assert!(!rule.pattern.contains("<0x0"));
            assert!(!rule.pattern.contains('\t'));
        }
        assert!(config.jstack_thread_name_filters.is_empty());
        assert!(config.jstack_stack_segment_filters.is_empty());
    }

    /// 验证旧版自由文本过滤配置在归一化时一次性迁移为规则列表。
    #[test]
    fn normalized_migrates_legacy_jstack_filters_into_rules() {
        let mut config = LogDisplayConfig {
            jstack_thread_filter_rules: Vec::new(),
            jstack_thread_name_filters: "Signal Dispatcher;VM Thread".to_string(),
            jstack_stack_segment_filters: "Unsafe.park||LockSupport.park".to_string(),
        };

        config.normalize_jstack_thread_filter_rules();

        let rules = &config.jstack_thread_filter_rules;
        assert_eq!(rules.len(), 4);
        assert_eq!(rules[0].pattern, "Signal Dispatcher");
        assert_eq!(rules[1].pattern, "VM Thread");
        assert_eq!(rules[2].kind, JstackThreadFilterRuleKind::StackSegment);
        assert_eq!(rules[2].pattern, "Unsafe.park");
        assert_eq!(rules[3].pattern, "LockSupport.park");
        assert!(config.jstack_thread_name_filters.is_empty());
        assert!(config.jstack_stack_segment_filters.is_empty());
    }

    /// 验证已有规则列表的配置不会被旧字段残留值覆盖。
    #[test]
    fn normalized_keeps_existing_rules_and_drops_legacy_values() {
        let mut config = LogDisplayConfig {
            jstack_thread_filter_rules: vec![JstackThreadFilterRule {
                enabled: false,
                kind: JstackThreadFilterRuleKind::ThreadName,
                pattern: "custom".to_string(),
            }],
            jstack_thread_name_filters: "stale".to_string(),
            jstack_stack_segment_filters: "stale block".to_string(),
        };

        config.normalize_jstack_thread_filter_rules();

        assert_eq!(config.jstack_thread_filter_rules.len(), 1);
        assert_eq!(config.jstack_thread_filter_rules[0].pattern, "custom");
        assert!(config.jstack_thread_name_filters.is_empty());
        assert!(config.jstack_stack_segment_filters.is_empty());
    }
}
