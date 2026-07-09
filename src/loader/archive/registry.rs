//! 文件职责：维护压缩格式适配器注册表。
//! 创建日期：2026-06-10
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：统一压缩包格式识别、能力声明查询、条目枚举、条目读取和错误上下文包装。

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Context as _, Result};

use crate::loader::archive::adapter::{
    ArchiveAdapter, ArchiveCapabilities, ArchiveEntryConsumer, ArchiveEntryInfo, ArchiveReadSeek,
    ArchiveRootProbe,
};
use crate::loader::archive::compressed_tar::CompressedTarArchiveAdapter;
use crate::loader::archive::detector::ArchiveFormat;
use crate::loader::archive::gzip_adapter::GzipArchiveAdapter;
use crate::loader::archive::password::{ArchivePasswordKey, annotate_archive_password_error};
use crate::loader::archive::rar_adapter::RarArchiveAdapter;
use crate::loader::archive::sevenz_adapter::SevenzArchiveAdapter;
use crate::loader::archive::tar_adapter::TarArchiveAdapter;
use crate::loader::archive::zip_adapter::ZipArchiveAdapter;

/// ZIP 内置适配器实例；注册表只持有共享引用，避免重复分配。
static ZIP_ADAPTER: ZipArchiveAdapter = ZipArchiveAdapter;
/// TAR 内置适配器实例。
static TAR_ADAPTER: TarArchiveAdapter = TarArchiveAdapter;
/// tar.gz 内置适配器实例。
static TAR_GZ_ADAPTER: CompressedTarArchiveAdapter = CompressedTarArchiveAdapter {
    format: ArchiveFormat::TarGz,
};
/// tar.bz2 内置适配器实例。
static TAR_BZ2_ADAPTER: CompressedTarArchiveAdapter = CompressedTarArchiveAdapter {
    format: ArchiveFormat::TarBz2,
};
/// tar.xz 内置适配器实例。
static TAR_XZ_ADAPTER: CompressedTarArchiveAdapter = CompressedTarArchiveAdapter {
    format: ArchiveFormat::TarXz,
};
/// 普通 gzip 内置适配器实例。
static GZIP_ADAPTER: GzipArchiveAdapter = GzipArchiveAdapter;
/// 7Z 内置适配器实例。
static SEVENZ_ADAPTER: SevenzArchiveAdapter = SevenzArchiveAdapter;
/// RAR 内置适配器实例。
static RAR_ADAPTER: RarArchiveAdapter = RarArchiveAdapter;

/// 默认压缩适配器注册表；初始化后进程内复用。
static DEFAULT_REGISTRY: OnceLock<ArchiveAdapterRegistry> = OnceLock::new();

/// 获取全局默认压缩适配器注册表。
pub fn archive_registry() -> &'static ArchiveAdapterRegistry {
    DEFAULT_REGISTRY.get_or_init(ArchiveAdapterRegistry::with_builtin_adapters)
}

/// 压缩适配器注册表；后续新增格式只需注册新的适配器对象。
#[derive(Clone)]
pub struct ArchiveAdapterRegistry {
    /// 已注册适配器列表，顺序决定扩展名识别的兜底优先级。
    adapters: Vec<&'static dyn ArchiveAdapter>,
}

impl ArchiveAdapterRegistry {
    /// 构造包含所有内置格式的注册表。
    pub fn with_builtin_adapters() -> Self {
        Self {
            adapters: vec![
                &ZIP_ADAPTER,
                &TAR_GZ_ADAPTER,
                &TAR_BZ2_ADAPTER,
                &TAR_XZ_ADAPTER,
                &TAR_ADAPTER,
                &GZIP_ADAPTER,
                &SEVENZ_ADAPTER,
                &RAR_ADAPTER,
            ],
        }
    }

    /// 根据格式返回对应适配器。
    pub fn adapter_for(&self, format: ArchiveFormat) -> Option<&'static dyn ArchiveAdapter> {
        self.adapters
            .iter()
            .copied()
            .find(|adapter| adapter.capabilities().format == format)
    }

    /// 查询某格式的能力声明。
    pub fn capabilities(&self, format: ArchiveFormat) -> Option<ArchiveCapabilities> {
        self.adapter_for(format)
            .map(|adapter| adapter.capabilities())
    }

    /// 判断指定格式是否具备目录树展开所需的基本能力。
    pub fn is_supported(&self, format: ArchiveFormat) -> bool {
        self.capabilities(format).is_some_and(|capabilities| {
            capabilities.supports_listing
                && capabilities.supports_entry_reading
                && capabilities.supports_nested_archives
        })
    }

    /// 识别本地路径对应的压缩格式，明确容器文件头优先，扩展名兜底。
    pub fn detect_path(&self, path: &Path) -> Option<ArchiveFormat> {
        let name_format = path
            .file_name()
            .and_then(|file_name| file_name.to_str())
            .and_then(|name| self.detect_name(name));
        let header_format = self.detect_path_by_header(path);

        match header_format {
            Some(format) if requires_name_confirmation(format) && name_format == Some(format) => {
                Some(format)
            }
            Some(format) if requires_name_confirmation(format) => name_format,
            Some(format) => Some(format),
            None => name_format,
        }
    }

    /// 根据文件名或压缩包内虚拟条目名称识别格式。
    pub fn detect_name(&self, name: &str) -> Option<ArchiveFormat> {
        let lowercase_name = name.to_ascii_lowercase();
        self.adapters
            .iter()
            .copied()
            .find(|adapter| adapter.matches_name(&lowercase_name))
            .map(|adapter| adapter.capabilities().format)
    }

    /// 枚举本地压缩包条目并统一补充错误上下文。
    pub fn list_entries(
        &self,
        format: ArchiveFormat,
        path: &Path,
        password: Option<&str>,
    ) -> Result<Vec<ArchiveEntryInfo>> {
        let adapter = self.require_adapter(format)?;
        let label = adapter.capabilities().label;
        adapter
            .list_entries(path, password)
            .with_context(|| format!("{label} 条目枚举失败：{}", path.display()))
    }

    /// 枚举本地压缩包条目，并在密码失败时补充具体容器键。
    pub fn list_entries_with_password_context(
        &self,
        format: ArchiveFormat,
        path: &Path,
        password: Option<&str>,
        password_key: ArchivePasswordKey,
        source_label: String,
    ) -> Result<Vec<ArchiveEntryInfo>> {
        self.list_entries(format, path, password)
            .map_err(|error| annotate_archive_password_error(error, password_key, source_label))
    }

    /// 枚举内存压缩包条目并统一补充错误上下文。
    pub fn list_entries_from_reader(
        &self,
        format: ArchiveFormat,
        reader: &mut dyn ArchiveReadSeek,
        reader_len: u64,
        source_label: &str,
        password: Option<&str>,
    ) -> Result<Vec<ArchiveEntryInfo>> {
        let adapter = self.require_adapter(format)?;
        let label = adapter.capabilities().label;
        adapter
            .list_entries_from_reader(reader, reader_len, source_label, password)
            .with_context(|| format!("{label} 内存条目枚举失败：{source_label}"))
    }

    /// 枚举内存压缩包条目，并在密码失败时补充具体容器键。
    pub fn list_entries_from_reader_with_password_context(
        &self,
        format: ArchiveFormat,
        reader: &mut dyn ArchiveReadSeek,
        reader_len: u64,
        source_label: &str,
        password: Option<&str>,
        password_key: ArchivePasswordKey,
    ) -> Result<Vec<ArchiveEntryInfo>> {
        self.list_entries_from_reader(format, reader, reader_len, source_label, password)
            .map_err(|error| {
                annotate_archive_password_error(error, password_key, source_label.to_string())
            })
    }

    /// 轻量探测本地压缩包根层是否恰好只有一个普通文件。
    pub fn probe_single_file_root(
        &self,
        format: ArchiveFormat,
        path: &Path,
        password: Option<&str>,
    ) -> Result<ArchiveRootProbe> {
        let adapter = self.require_adapter(format)?;
        let label = adapter.capabilities().label;
        adapter
            .probe_single_file_root(path, password)
            .with_context(|| format!("{label} 根层单文件探测失败：{}", path.display()))
    }

    /// 轻量探测本地压缩包根层是否恰好只有一个普通文件，并补充密码上下文。
    pub fn probe_single_file_root_with_password_context(
        &self,
        format: ArchiveFormat,
        path: &Path,
        password: Option<&str>,
        password_key: ArchivePasswordKey,
        source_label: String,
    ) -> Result<ArchiveRootProbe> {
        self.probe_single_file_root(format, path, password)
            .map_err(|error| annotate_archive_password_error(error, password_key, source_label))
    }

    /// 轻量探测内存压缩包根层是否恰好只有一个普通文件。
    pub fn probe_single_file_root_from_reader(
        &self,
        format: ArchiveFormat,
        reader: &mut dyn ArchiveReadSeek,
        reader_len: u64,
        source_label: &str,
        password: Option<&str>,
    ) -> Result<ArchiveRootProbe> {
        let adapter = self.require_adapter(format)?;
        let label = adapter.capabilities().label;
        adapter
            .probe_single_file_root_from_reader(reader, reader_len, source_label, password)
            .with_context(|| format!("{label} 内存根层单文件探测失败：{source_label}"))
    }

    /// 轻量探测内存压缩包根层是否恰好只有一个普通文件，并补充密码上下文。
    pub fn probe_single_file_root_from_reader_with_password_context(
        &self,
        format: ArchiveFormat,
        reader: &mut dyn ArchiveReadSeek,
        reader_len: u64,
        source_label: &str,
        password: Option<&str>,
        password_key: ArchivePasswordKey,
    ) -> Result<ArchiveRootProbe> {
        self.probe_single_file_root_from_reader(format, reader, reader_len, source_label, password)
            .map_err(|error| {
                annotate_archive_password_error(error, password_key, source_label.to_string())
            })
    }

    /// 从本地压缩包读取指定条目并统一补充错误上下文。
    pub fn read_entry_bytes(
        &self,
        format: ArchiveFormat,
        path: &Path,
        entry_path: &str,
        password: Option<&str>,
        password_key: ArchivePasswordKey,
        source_label: String,
    ) -> Result<Vec<u8>> {
        let adapter = self.require_adapter(format)?;
        let label = adapter.capabilities().label;
        adapter
            .read_entry_bytes(path, entry_path, password)
            .with_context(|| format!("{label} 条目读取失败：{}!/{entry_path}", path.display()))
            .map_err(|error| annotate_archive_password_error(error, password_key, source_label))
    }

    /// 从内存压缩包读取指定条目并统一补充错误上下文。
    pub fn read_entry_bytes_from_reader(
        &self,
        format: ArchiveFormat,
        reader: &mut dyn ArchiveReadSeek,
        reader_len: u64,
        entry_path: &str,
        source_label: &str,
        password: Option<&str>,
        password_key: ArchivePasswordKey,
    ) -> Result<Vec<u8>> {
        let adapter = self.require_adapter(format)?;
        let label = adapter.capabilities().label;
        adapter
            .read_entry_bytes_from_reader(reader, reader_len, entry_path, source_label, password)
            .with_context(|| format!("{label} 内存条目读取失败：{source_label}!/{entry_path}"))
            .map_err(|error| {
                annotate_archive_password_error(error, password_key, source_label.to_string())
            })
    }

    /// 从本地压缩包流式读取指定条目并统一补充错误上下文。
    pub fn stream_entry(
        &self,
        format: ArchiveFormat,
        path: &Path,
        entry_path: &str,
        password: Option<&str>,
        password_key: ArchivePasswordKey,
        source_label: String,
        consumer: &mut ArchiveEntryConsumer<'_>,
    ) -> Result<()> {
        let adapter = self.require_adapter(format)?;
        let label = adapter.capabilities().label;
        adapter
            .stream_entry(path, entry_path, password, consumer)
            .with_context(|| format!("{label} 条目流式读取失败：{}!/{entry_path}", path.display()))
            .map_err(|error| annotate_archive_password_error(error, password_key, source_label))
    }

    /// 从内存压缩包流式读取指定条目并统一补充错误上下文。
    pub fn stream_entry_from_reader(
        &self,
        format: ArchiveFormat,
        reader: &mut dyn ArchiveReadSeek,
        reader_len: u64,
        entry_path: &str,
        source_label: &str,
        password: Option<&str>,
        password_key: ArchivePasswordKey,
        consumer: &mut ArchiveEntryConsumer<'_>,
    ) -> Result<()> {
        let adapter = self.require_adapter(format)?;
        let label = adapter.capabilities().label;
        adapter
            .stream_entry_from_reader(
                reader,
                reader_len,
                entry_path,
                source_label,
                password,
                consumer,
            )
            .with_context(|| format!("{label} 内存条目流式读取失败：{source_label}!/{entry_path}"))
            .map_err(|error| {
                annotate_archive_password_error(error, password_key, source_label.to_string())
            })
    }

    /// 通过文件头识别本地压缩包格式。
    fn detect_path_by_header(&self, path: &Path) -> Option<ArchiveFormat> {
        let mut file = File::open(path).ok()?;
        let mut header = [0_u8; 512];
        let bytes_read = file.read(&mut header).ok()?;
        let sample = &header[..bytes_read];

        self.adapters
            .iter()
            .copied()
            .filter(|adapter| adapter.capabilities().supports_header_detection)
            .find(|adapter| adapter.matches_header(sample))
            .map(|adapter| adapter.capabilities().format)
    }

    /// 获取已注册适配器，未注册时生成统一错误。
    fn require_adapter(&self, format: ArchiveFormat) -> Result<&'static dyn ArchiveAdapter> {
        self.adapter_for(format)
            .with_context(|| format!("未注册压缩格式适配器：{format:?}"))
    }
}

/// 返回文件头只能说明外层压缩编码，仍需扩展名确认其内部确实应被当作 TAR 容器的格式。
fn requires_name_confirmation(format: ArchiveFormat) -> bool {
    matches!(
        format,
        ArchiveFormat::TarGz | ArchiveFormat::TarBz2 | ArchiveFormat::TarXz
    )
}

#[cfg(test)]
mod tests {
    use super::ArchiveAdapterRegistry;
    use crate::loader::archive::detector::ArchiveFormat;
    use std::fs;
    use std::path::PathBuf;

    /// 构造压缩格式注册表测试文件路径，避免依赖真实用户目录。
    fn test_archive_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "argus-archive-registry-{}-{name}",
            std::process::id()
        ))
    }

    /// 验证注册表可以通过扩展名识别所有内置格式。
    #[test]
    fn detects_builtin_formats_by_name() {
        let registry = ArchiveAdapterRegistry::with_builtin_adapters();

        assert_eq!(registry.detect_name("app.zip"), Some(ArchiveFormat::Zip));
        assert_eq!(registry.detect_name("logs.tar"), Some(ArchiveFormat::Tar));
        assert_eq!(
            registry.detect_name("logs.tar.gz"),
            Some(ArchiveFormat::TarGz)
        );
        assert_eq!(
            registry.detect_name("logs.tbz2"),
            Some(ArchiveFormat::TarBz2)
        );
        assert_eq!(registry.detect_name("logs.txz"), Some(ArchiveFormat::TarXz));
        assert_eq!(
            registry.detect_name("app.log.gz"),
            Some(ArchiveFormat::Gzip)
        );
        assert_eq!(registry.detect_name("logs.7z"), Some(ArchiveFormat::SevenZ));
        assert_eq!(registry.detect_name("logs.rar"), Some(ArchiveFormat::Rar));
        assert_eq!(registry.detect_name("app.log"), None);
    }

    /// 验证能力声明由注册表统一提供，避免 UI 或加载器自行硬编码支持范围。
    #[test]
    fn exposes_capabilities_for_registered_format() {
        let registry = ArchiveAdapterRegistry::with_builtin_adapters();
        let capabilities = registry
            .capabilities(ArchiveFormat::Zip)
            .expect("ZIP 应该注册能力声明");

        assert_eq!(capabilities.label, "ZIP");
        assert!(capabilities.supports_listing);
        assert!(capabilities.supports_entry_reading);
        assert!(capabilities.supports_nested_archives);
        assert!(capabilities.supports_passwords);
    }

    /// 验证普通 gzip 文件不会只因 gzip 魔数被误判成 tar.gz 目录树。
    #[test]
    fn gzip_header_without_tar_extension_is_not_detected_as_tar_gz() {
        let registry = ArchiveAdapterRegistry::with_builtin_adapters();
        let path = test_archive_path("app.log.gz");
        fs::write(&path, [0x1F, 0x8B, 0x08, 0x00]).expect("应能写入 gzip 头测试文件");

        assert_eq!(registry.detect_path(&path), Some(ArchiveFormat::Gzip));

        let _ = fs::remove_file(path);
    }

    /// 验证只有 gzip 文件头但没有 gzip 扩展名时不会误判为 tar.gz。
    #[test]
    fn gzip_header_without_gzip_name_is_not_detected_as_tar_gz() {
        let registry = ArchiveAdapterRegistry::with_builtin_adapters();
        let path = test_archive_path("app.log");
        fs::write(&path, [0x1F, 0x8B, 0x08, 0x00]).expect("应能写入 gzip 头测试文件");

        assert_eq!(registry.detect_path(&path), None);

        let _ = fs::remove_file(path);
    }
}
