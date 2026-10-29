//! 文件职责：定义压缩包统一适配器抽象。
//! 创建日期：2026-06-09
//! 修改日期：2026-10-27
//! 作者：Argus 开发团队
//! 主要功能：为 ZIP、TAR、压缩 TAR、7Z 和 RAR 等格式提供统一识别、枚举、单条读取、会话式读取和能力声明模型。

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

use anyhow::{Context as _, Result, bail};

use crate::loader::archive::detector::ArchiveFormat;

/// 压缩包条目枚举结果；只保存结构信息，不读取日志正文内容。
#[derive(Clone, Debug)]
pub(crate) struct ArchiveEntryInfo {
    /// 压缩包内规范化路径，统一使用 `/` 分隔。
    pub path: String,
    /// 是否为目录条目。
    pub is_dir: bool,
    /// 解压后的大小（字节）；格式无法在包头给出时为 `None`，此时只能解压后校验。
    pub unpacked_size: Option<u64>,
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

/// 一次顺序提取中已完成路径校验和命名分配的条目。
pub(crate) struct ArchiveEntryTarget {
    pub entry_path: String,
    pub target: PathBuf,
}

/// 标记“单个条目无法解码、可以跳过该条目继续加载”的错误。
///
/// 说明：只有适配器能明确区分条目级解码失败（数据损坏、解码资源上限）与目标
/// 文件写入失败时才使用该标记。调用方据此把该条目降级为警告，既避免一个坏条目
/// 让整次加载失败，也不会把磁盘写满之类的问题掩盖成警告。
#[derive(Debug)]
pub(crate) struct SkippableEntryError {
    /// 真实解码失败原因。
    source: anyhow::Error,
}

impl SkippableEntryError {
    /// 把条目级解码失败包装为可跳过错误。
    pub(crate) fn new(source: anyhow::Error) -> Self {
        Self { source }
    }
}

impl std::fmt::Display for SkippableEntryError {
    /// 直接输出下层失败原因，保留用户在提示里看到的细节。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.source)
    }
}

impl std::error::Error for SkippableEntryError {
    /// 保留下层错误链，便于用 `{:#}` 输出完整原因。
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// 判断错误是否为“可跳过条目”的标记错误。
pub(crate) fn is_skippable_entry_error(error: &anyhow::Error) -> bool {
    error.downcast_ref::<SkippableEntryError>().is_some()
}

/// 一次打开压缩包后的会话式条目读取。
///
/// 条目数量大的压缩包（上万个条目、大量嵌套包）如果逐条目重新打开并重解析
/// 中央目录，开销会随条目数平方增长；会话保证整个解压过程只解析一次。
pub(crate) trait ArchiveEntrySession: Send {
    /// 枚举全部条目；语义与 [`ArchiveAdapter::list_entries`] 一致。
    fn list_entries(&mut self) -> Result<Vec<ArchiveEntryInfo>>;

    /// 流式输出一个条目；语义与 [`ArchiveAdapter::stream_entry`] 一致。
    fn stream_entry(
        &mut self,
        entry_path: &str,
        consumer: &mut ArchiveEntryConsumer<'_>,
    ) -> Result<()>;
}

/// 压缩包适配器统一接口；每个格式自行声明识别规则、能力和读写入口。
pub(crate) trait ArchiveAdapter: Sync {
    /// 返回当前适配器的能力声明。
    fn capabilities(&self) -> ArchiveCapabilities;

    /// 顺序容器必须一次遍历提取，避免每个条目重新解压前面的全部内容。
    fn supports_batch_extraction(&self) -> bool {
        false
    }

    /// 提取一批已校验条目；不支持该能力的适配器必须明确报错。
    ///
    /// 参数说明：
    /// - `path`：本地压缩包路径。
    /// - `targets`：需要提取的条目及目标路径。
    /// - `password`：解密密码；无加密时为 `None`。
    /// - `max_log_bytes`：单个条目解压后的最大字节数。
    /// - `cancellation`：取消令牌。
    fn extract_entries_to_paths(
        &self,
        _path: &Path,
        _targets: &[ArchiveEntryTarget],
        _password: Option<&str>,
        _max_log_bytes: u64,
        _cancellation: &CancellationToken,
    ) -> Result<()> {
        bail!("当前归档格式不支持顺序批量提取")
    }

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

    /// 把指定条目直接流式写入目标文件。
    ///
    /// 默认实现先创建目标文件，再通过 [`ArchiveAdapter::stream_entry`] 顺序追加。
    /// 支持大压缩比或带过滤器的格式（如 RAR5）应覆盖本方法，让解码库直接写文件，
    /// 避免整个条目驻留内存。
    fn extract_entry_to_path(
        &self,
        path: &Path,
        entry_path: &str,
        password: Option<&str>,
        target: &Path,
        max_log_bytes: u64,
    ) -> Result<()> {
        let mut file = File::create(target)
            .with_context(|| format!("无法创建物化文件：{}", target.display()))?;
        let mut written = 0_u64;
        self.stream_entry(path, entry_path, password, &mut |chunk| {
            written = written.saturating_add(chunk.len() as u64);
            if written > max_log_bytes {
                bail!("解压后超过最大日志大小 {max_log_bytes} 字节");
            }
            file.write_all(chunk)?;
            Ok(())
        })
    }

    /// 打开压缩包建立会话式读取；返回 `None` 时调用方回退逐条目打开的旧路径。
    ///
    /// 默认不提供会话；支持会话是纯性能优化，语义必须与逐条目接口完全一致。
    fn open_session(
        &self,
        _path: &Path,
        _password: Option<&str>,
    ) -> Result<Option<Box<dyn ArchiveEntrySession>>> {
        Ok(None)
    }
}
