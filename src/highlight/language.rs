//! 文件职责：识别日志阅读区当前行所属的高亮语言。
//! 创建日期：2026-06-11
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：根据标签标题和来源路径后缀选择日志、配置文件或 Java 线程栈高亮规则。

/// 当前行应该使用的高亮语言。
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HighlightLanguage {
    /// 普通日志。
    Log,
    /// Java 线程栈或 jstack 输出。
    JavaThreadDump,
    /// Properties、INI、CONF 等 key-value 配置。
    Properties,
    /// XML、XSD、WSDL 等标记语言。
    Xml,
    /// JSON 配置。
    Json,
    /// YAML 配置。
    Yaml,
    /// 不执行语法高亮。
    Plain,
}

/// 根据标签标题和路径识别高亮语言。
///
/// 参数说明：
/// - `label`：标签页标题，通常是文件名。
/// - `path`：真实来源路径或压缩包内虚拟路径。
///
/// 返回值：当前来源最合适的高亮语言；未知后缀默认按普通日志处理。
pub fn detect_highlight_language(label: &str, path: &str) -> HighlightLanguage {
    let name = if path.is_empty() { label } else { path };
    let lower = name.to_ascii_lowercase();

    if ends_with_any(&lower, &[".tdump", ".jstack", ".thread", ".threads"]) {
        HighlightLanguage::JavaThreadDump
    } else if ends_with_any(&lower, &[".properties", ".conf", ".ini", ".cfg"]) {
        HighlightLanguage::Properties
    } else if ends_with_any(&lower, &[".xml", ".xsd", ".wsdl"]) {
        HighlightLanguage::Xml
    } else if lower.ends_with(".json") {
        HighlightLanguage::Json
    } else if ends_with_any(&lower, &[".yaml", ".yml"]) {
        HighlightLanguage::Yaml
    } else {
        HighlightLanguage::Log
    }
}

/// 判断文件名是否匹配任意后缀。
fn ends_with_any(text: &str, suffixes: &[&str]) -> bool {
    suffixes.iter().any(|suffix| text.ends_with(suffix))
}
