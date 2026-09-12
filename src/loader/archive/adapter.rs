//! 文件职责：定义压缩包统一适配器抽象。
//! 创建日期：2026-06-09
//! 修改日期：2026-09-12
//! 作者：Argus 开发团队
//! 主要功能：为 ZIP、TAR、压缩 TAR、7Z 和 RAR 等格式提供统一识别、枚举、单条读取和能力声明模型。

use std::path::Path;

use anyhow::Result;

use crate::loader::archive::detector::ArchiveFormat;

/// 压缩包条目枚举结果；只保存结构信息，不读取日志正文内容。
#[derive(Clone, Debug)]
pub(crate) struct ArchiveEntryInfo {
    /// 压缩包内规范化路径，统一使用 `/` 分隔。
    pub path: String,
    /// 是否为目录条目。
    pub is_dir: bool,
    /// 条目未压缩大小；部分格式可能无法提供。
    pub size: Option<u64>,
}

/// 压缩格式能力声明，供 UI、加载器和后续格式扩展判断可用能力。
#[derive(Clone, Copy, Debug)]
pub(crate) struct ArchiveCapabilities {
    /// 该能力声明对应的压缩格式。
    pub format: ArchiveFormat,
    /// 面向用户展示的格式名称。
    pub label: &'static str,
    /// 可通过文件名识别的扩展名列表，包含前导点并按完整扩展名书写。
    pub extensions: &'static [&'static str],
    /// 是否支持通过文件头识别格式。
    pub supports_header_detection: bool,
    /// 是否支持枚举压缩包条目。
    pub supports_listing: bool,
    /// 是否支持读取单个条目字节。
    pub supports_entry_reading: bool,
    /// 是否支持作为嵌套压缩包继续展开。
    pub supports_nested_archives: bool,
}

/// 压缩包条目流式输出回调；适配器每读取到一段解压后字节就调用一次。
pub(crate) type ArchiveEntryConsumer<'a> = dyn FnMut(&[u8]) -> Result<()> + 'a;

/// 压缩包适配器统一接口；每个格式自行声明识别规则、能力和读写入口。
pub(crate) trait ArchiveAdapter: Sync {
    /// 返回当前适配器的能力声明。
    fn capabilities(&self) -> ArchiveCapabilities;

    /// 判断文件头样本是否匹配当前压缩格式。
    fn matches_header(&self, _sample: &[u8]) -> bool {
        false
    }

    /// 判断已转为小写的文件名是否匹配当前格式扩展名。
    fn matches_name(&self, lowercase_name: &str) -> bool {
        self.capabilities()
            .extensions
            .iter()
            .any(|extension| lowercase_name.ends_with(extension))
    }

    /// 枚举本地压缩包条目。
    ///
    /// 参数说明：
    /// - `path`：本地压缩包路径。
    ///
    /// 返回值：压缩包内条目列表；不执行正文读取或解压到磁盘。
    fn list_entries(&self, path: &Path, password: Option<&str>) -> Result<Vec<ArchiveEntryInfo>>;

    /// 从本地压缩包读取指定条目的完整字节。
    ///
    /// 返回值：目标条目原始字节；用于嵌套压缩包继续解析。
    fn read_entry_bytes(
        &self,
        path: &Path,
        entry_path: &str,
        password: Option<&str>,
    ) -> Result<Vec<u8>>;

    /// 从本地压缩包流式输出指定条目内容。
    ///
    /// 默认实现会复用完整字节读取能力，保证新增格式只实现旧接口也能工作；
    /// ZIP、TAR、压缩 TAR、7Z 等内置适配器会覆盖为真正的 chunk 回调。
    fn stream_entry(
        &self,
        path: &Path,
        entry_path: &str,
        password: Option<&str>,
        consumer: &mut ArchiveEntryConsumer<'_>,
    ) -> Result<()> {
        let bytes = self.read_entry_bytes(path, entry_path, password)?;
        consumer(&bytes)
    }
}
