//! 文件职责：定义应用运行期配置模型。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：提供日志来源加载、编码、缓存等模块所需的默认配置。

/// 应用配置根对象，当前只保存内存默认值，不执行磁盘持久化。
#[derive(Clone, Debug, Default)]
pub struct AppConfig {
    /// 日志来源加载配置，控制目录和压缩包的展开策略。
    pub loader: LoaderConfig,
}

/// 日志来源加载配置，用于限制高成本文件系统和压缩包操作。
#[derive(Clone, Debug)]
pub struct LoaderConfig {
    /// 允许展开的嵌套压缩包最大层级；MVP 固定为 2，后续可外置为用户配置。
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
