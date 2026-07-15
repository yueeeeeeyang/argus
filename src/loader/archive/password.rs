//! 文件职责：定义压缩包密码上下文、会话缓存和统一密码错误模型。
//! 创建日期：2026-07-08
//! 修改日期：2026-07-08
//! 作者：Argus 开发团队
//! 主要功能：为 ZIP、7Z、RAR 等支持加密的压缩格式提供统一密码索引和错误识别能力。

use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::utils::path::{archive_virtual_path, normalize_archive_entry_path};

/// 压缩包密码缓存键；同一个真实文件里的不同嵌套容器可能使用不同密码。
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ArchivePasswordKey {
    /// 最外层真实压缩包路径。
    pub archive_path: PathBuf,
    /// 当前需要解锁的容器链路；最外层容器为空，内嵌容器包含从外到内的条目路径。
    pub container_entries: Vec<String>,
}

impl ArchivePasswordKey {
    /// 构造密码键，并统一规范化内嵌容器路径。
    pub(crate) fn new(archive_path: impl Into<PathBuf>, container_entries: &[String]) -> Self {
        Self {
            archive_path: archive_path.into(),
            container_entries: container_entries
                .iter()
                .map(|entry| normalize_archive_entry_path(entry))
                .collect(),
        }
    }

    /// 构造最外层真实压缩包的密码键。
    pub(crate) fn root(archive_path: impl Into<PathBuf>) -> Self {
        Self {
            archive_path: archive_path.into(),
            container_entries: Vec::new(),
        }
    }

    /// 返回面向用户展示的容器路径。
    pub(crate) fn display_label(&self) -> String {
        if self.container_entries.is_empty() {
            return self.archive_path.display().to_string();
        }

        let Some((entry_path, parent_containers)) = self.container_entries.split_last() else {
            return self.archive_path.display().to_string();
        };
        archive_virtual_path(&self.archive_path, parent_containers, entry_path)
    }
}

/// 当前进程内的压缩包密码缓存；只保存在内存，不写入配置文件。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ArchivePasswordStore {
    /// 按容器键保存的明文密码；作用域仅限当前 Argus 进程。
    passwords: HashMap<ArchivePasswordKey, String>,
}

impl ArchivePasswordStore {
    /// 返回缓存中是否没有任何密码。
    pub(crate) fn is_empty(&self) -> bool {
        self.passwords.is_empty()
    }

    /// 保存或替换某个压缩包容器的密码。
    pub(crate) fn insert(&mut self, key: ArchivePasswordKey, password: String) {
        self.passwords.insert(key, password);
    }

    /// 移除某个压缩包容器已缓存的密码。
    pub(crate) fn remove(&mut self, key: &ArchivePasswordKey) {
        self.passwords.remove(key);
    }

    /// 读取指定容器的密码。
    pub(crate) fn get(&self, key: &ArchivePasswordKey) -> Option<&str> {
        self.passwords.get(key).map(String::as_str)
    }

    /// 根据真实压缩包路径和容器链路读取密码。
    pub(crate) fn get_for_container(
        &self,
        archive_path: &Path,
        container_entries: &[String],
    ) -> Option<&str> {
        let key = ArchivePasswordKey::new(archive_path.to_path_buf(), container_entries);
        self.get(&key)
    }
}

/// 压缩包密码失败类型，供 UI 判断应该弹窗还是展示普通错误。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArchivePasswordErrorKind {
    /// 目标压缩包需要密码，但当前没有可用密码。
    Required,
    /// 已提供密码，但底层解密校验失败。
    Invalid,
    /// 当前格式或算法是加密压缩包，但底层库无法支持。
    Unsupported,
}

/// 压缩包密码相关错误；key 为空时表示来自格式适配器的底层错误，注册表会补充具体容器键。
#[derive(Clone, Debug)]
pub(crate) struct ArchivePasswordError {
    /// 密码失败类型。
    pub kind: ArchivePasswordErrorKind,
    /// 触发错误的压缩包容器键。
    pub key: Option<ArchivePasswordKey>,
    /// 面向用户展示的容器路径。
    pub source_label: String,
    /// 底层库返回的额外说明。
    pub detail: Option<String>,
}

impl ArchivePasswordError {
    /// 构造缺少密码错误。
    pub(crate) fn required(source_label: impl Into<String>) -> Self {
        Self {
            kind: ArchivePasswordErrorKind::Required,
            key: None,
            source_label: source_label.into(),
            detail: None,
        }
    }

    /// 构造密码错误。
    pub(crate) fn invalid(source_label: impl Into<String>) -> Self {
        Self {
            kind: ArchivePasswordErrorKind::Invalid,
            key: None,
            source_label: source_label.into(),
            detail: None,
        }
    }

    /// 构造不支持的加密算法错误。
    pub(crate) fn unsupported(source_label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            kind: ArchivePasswordErrorKind::Unsupported,
            key: None,
            source_label: source_label.into(),
            detail: Some(detail.into()),
        }
    }

    /// 给底层密码错误补充容器键和更准确的展示路径。
    pub(crate) fn with_context(
        mut self,
        key: ArchivePasswordKey,
        source_label: impl Into<String>,
    ) -> Self {
        self.key = Some(key);
        self.source_label = source_label.into();
        self
    }

    /// 返回当前错误是否表示密码错误而非首次缺少密码。
    pub(crate) fn is_invalid_password(&self) -> bool {
        self.kind == ArchivePasswordErrorKind::Invalid
    }
}

impl fmt::Display for ArchivePasswordError {
    /// 输出用户可理解的密码错误说明。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            ArchivePasswordErrorKind::Required => {
                write!(formatter, "压缩包需要密码：{}", self.source_label)
            }
            ArchivePasswordErrorKind::Invalid => {
                write!(formatter, "压缩包密码错误：{}", self.source_label)
            }
            ArchivePasswordErrorKind::Unsupported => {
                let detail = self.detail.as_deref().unwrap_or("当前加密算法暂不支持");
                write!(
                    formatter,
                    "压缩包加密方式暂不支持：{}（{}）",
                    self.source_label, detail
                )
            }
        }
    }
}

impl Error for ArchivePasswordError {}

/// 从 anyhow 错误链中提取压缩包密码错误。
pub(crate) fn find_archive_password_error(error: &anyhow::Error) -> Option<ArchivePasswordError> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<ArchivePasswordError>().cloned())
}

/// 若错误链中包含密码错误，则补充当前容器上下文；否则保留原错误。
pub(crate) fn annotate_archive_password_error(
    error: anyhow::Error,
    key: ArchivePasswordKey,
    source_label: impl Into<String>,
) -> anyhow::Error {
    match find_archive_password_error(&error) {
        Some(password_error) => password_error.with_context(key, source_label).into(),
        None => error,
    }
}
