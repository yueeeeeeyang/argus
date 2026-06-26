//! 文件职责：维护链接工作区的持久化配置、目录树索引和表单校验规则。
//! 创建日期：2026-06-26
//! 修改日期：2026-06-26
//! 作者：Argus 开发团队
//! 主要功能：提供 SSH 链接目录、同级重名校验、过滤可见行和受信主机指纹配置。

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 链接目录和链接共享的节点 ID 类型，便于 UI 在同一棵树中统一选中、展开和定位。
pub type ConnectionNodeId = usize;

/// SSH 连接默认端口；新增链接表单未填写端口时使用该值。
pub const DEFAULT_SSH_PORT: u16 = 22;

/// 链接工作区持久化配置，保存目录、SSH 链接和已确认的主机指纹。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ConnectionConfig {
    /// 下一个可分配的目录或链接 ID。
    #[serde(default = "default_next_connection_id")]
    pub next_id: ConnectionNodeId,
    /// 用户创建的链接目录，根目录不单独落盘，使用 `parent_id = None` 表示根层级。
    #[serde(default)]
    pub directories: Vec<ConnectionDirectoryConfig>,
    /// 用户创建的 SSH 链接。
    #[serde(default)]
    pub links: Vec<ConnectionLinkConfig>,
    /// 已经由用户确认可信的主机指纹。
    #[serde(default)]
    pub trusted_hosts: Vec<TrustedHostKeyConfig>,
}

impl Default for ConnectionConfig {
    /// 构造空链接配置，保证旧配置升级后链接工作区可直接打开。
    fn default() -> Self {
        Self {
            next_id: default_next_connection_id(),
            directories: Vec::new(),
            links: Vec::new(),
            trusted_hosts: Vec::new(),
        }
    }
}

impl ConnectionConfig {
    /// 返回经过边界修正后的配置副本，避免坏配置造成 ID 冲突或端口越界。
    pub fn normalized(mut self) -> Self {
        let mut used_ids = BTreeSet::new();
        self.directories.retain(|directory| {
            directory.id > 0 && used_ids.insert(directory.id) && !directory.name.trim().is_empty()
        });
        self.links.retain(|link| {
            link.id > 0
                && used_ids.insert(link.id)
                && !link.name.trim().is_empty()
                && !link.ssh.host.trim().is_empty()
                && !link.ssh.username.trim().is_empty()
        });
        let directory_ids = self
            .directories
            .iter()
            .map(|directory| directory.id)
            .collect::<BTreeSet<_>>();
        self.directories.retain(|directory| {
            directory
                .parent_id
                .is_none_or(|parent_id| directory_ids.contains(&parent_id))
        });
        self.links.retain(|link| {
            link.parent_id
                .is_none_or(|parent_id| directory_ids.contains(&parent_id))
        });
        for link in &mut self.links {
            link.name = normalized_required_text(&link.name);
            link.ssh.host = normalized_required_text(&link.ssh.host);
            link.ssh.username = normalized_required_text(&link.ssh.username);
            link.ssh.private_key_path = normalized_optional_text(link.ssh.private_key_path.take());
            link.ssh.private_key_passphrase =
                normalized_optional_secret_text(link.ssh.private_key_passphrase.take());
            if link.ssh.port == 0 {
                link.ssh.port = DEFAULT_SSH_PORT;
            }
        }
        for directory in &mut self.directories {
            directory.name = normalized_required_text(&directory.name);
        }
        self.trusted_hosts.retain(|host| {
            host.port > 0 && !host.host.trim().is_empty() && !host.fingerprint.trim().is_empty()
        });
        for host in &mut self.trusted_hosts {
            host.host = normalized_required_text(&host.host);
            host.fingerprint = normalized_required_text(&host.fingerprint);
        }
        self.next_id = self
            .next_id
            .max(used_ids.iter().next_back().copied().unwrap_or_default() + 1)
            .max(default_next_connection_id());
        self
    }

    /// 根据当前选中节点推导新增目录的父目录；仅选中目录时创建子目录。
    pub fn parent_for_new_directory(
        &self,
        selected_id: Option<ConnectionNodeId>,
    ) -> Option<ConnectionNodeId> {
        selected_id.filter(|selected_id| self.is_directory(*selected_id))
    }

    /// 根据当前选中节点推导新增链接的父目录；选中链接时使用其父目录。
    pub fn parent_for_new_link(
        &self,
        selected_id: Option<ConnectionNodeId>,
    ) -> Option<ConnectionNodeId> {
        self.parent_for_new_node(selected_id)
    }

    /// 创建目录并返回新目录 ID。
    pub fn add_directory(
        &mut self,
        parent_id: Option<ConnectionNodeId>,
        name: &str,
    ) -> Result<ConnectionNodeId, ConnectionValidationError> {
        let name = validate_node_name(name)?;
        self.validate_parent_directory(parent_id)?;
        self.validate_sibling_name_available(parent_id, &name, None)?;

        let id = self.take_next_id();
        self.directories.push(ConnectionDirectoryConfig {
            id,
            parent_id,
            name,
            expanded: true,
        });
        if let Some(parent_id) = parent_id
            && let Some(parent) = self.directory_mut(parent_id)
        {
            parent.expanded = true;
        }
        Ok(id)
    }

    /// 创建 SSH 链接并返回新链接 ID。
    pub fn add_ssh_link(
        &mut self,
        parent_id: Option<ConnectionNodeId>,
        name: &str,
        ssh: SshLinkConfig,
    ) -> Result<ConnectionNodeId, ConnectionValidationError> {
        let name = validate_node_name(name)?;
        let ssh = ssh.normalized_for_save()?;
        self.validate_parent_directory(parent_id)?;
        self.validate_sibling_name_available(parent_id, &name, None)?;

        let id = self.take_next_id();
        self.links.push(ConnectionLinkConfig {
            id,
            parent_id,
            name,
            ssh,
        });
        if let Some(parent_id) = parent_id
            && let Some(parent) = self.directory_mut(parent_id)
        {
            parent.expanded = true;
        }
        Ok(id)
    }

    /// 重命名目录；同级重名校验会忽略当前目录自身。
    pub fn update_directory(
        &mut self,
        directory_id: ConnectionNodeId,
        name: &str,
    ) -> Result<(), ConnectionValidationError> {
        let name = validate_node_name(name)?;
        let parent_id = self
            .directory(directory_id)
            .ok_or(ConnectionValidationError::NodeNotFound)?
            .parent_id;
        self.validate_sibling_name_available(parent_id, &name, Some(directory_id))?;
        let directory = self
            .directory_mut(directory_id)
            .ok_or(ConnectionValidationError::NodeNotFound)?;
        directory.name = name;
        Ok(())
    }

    /// 更新 SSH 链接名称和连接参数；同级重名校验会忽略当前链接自身。
    pub fn update_ssh_link(
        &mut self,
        link_id: ConnectionNodeId,
        name: &str,
        ssh: SshLinkConfig,
    ) -> Result<(), ConnectionValidationError> {
        let name = validate_node_name(name)?;
        let ssh = ssh.normalized_for_save()?;
        let parent_id = self
            .link(link_id)
            .ok_or(ConnectionValidationError::NodeNotFound)?
            .parent_id;
        self.validate_sibling_name_available(parent_id, &name, Some(link_id))?;
        let link = self
            .links
            .iter_mut()
            .find(|link| link.id == link_id)
            .ok_or(ConnectionValidationError::NodeNotFound)?;
        link.name = name;
        link.ssh = ssh;
        Ok(())
    }

    /// 删除目录或链接；目录必须为空，避免误删整棵子树。
    pub fn delete_node(
        &mut self,
        node_id: ConnectionNodeId,
    ) -> Result<ConnectionDeletedNodeKind, ConnectionValidationError> {
        if self.directory(node_id).is_some() {
            if !self.is_directory_empty(node_id) {
                return Err(ConnectionValidationError::DirectoryNotEmpty);
            }
            self.directories.retain(|directory| directory.id != node_id);
            return Ok(ConnectionDeletedNodeKind::Directory);
        }

        if self.link(node_id).is_some() {
            self.links.retain(|link| link.id != node_id);
            return Ok(ConnectionDeletedNodeKind::SshLink);
        }

        Err(ConnectionValidationError::NodeNotFound)
    }

    /// 切换目录展开状态；非目录节点返回 `false`。
    pub fn toggle_directory_expanded(&mut self, directory_id: ConnectionNodeId) -> bool {
        let Some(directory) = self.directory_mut(directory_id) else {
            return false;
        };
        directory.expanded = !directory.expanded;
        true
    }

    /// 收起所有目录并返回实际发生变化的目录数量。
    pub fn collapse_all(&mut self) -> usize {
        let mut collapsed_count = 0;
        for directory in &mut self.directories {
            if directory.expanded {
                directory.expanded = false;
                collapsed_count += 1;
            }
        }
        collapsed_count
    }

    /// 生成链接目录树的可见行；过滤模式会保留命中节点及其祖先。
    pub fn visible_rows(
        &self,
        query: &str,
        selected_id: Option<ConnectionNodeId>,
    ) -> Vec<ConnectionTreeRow> {
        let query = query.trim().to_ascii_lowercase();
        let is_filtering = !query.is_empty();
        let visible_ids = if is_filtering {
            self.filtered_node_ids(&query)
        } else {
            BTreeSet::new()
        };
        let child_index = self.child_index();
        let mut rows = Vec::new();
        self.collect_visible_rows(
            None,
            0,
            &child_index,
            &visible_ids,
            is_filtering,
            selected_id,
            &mut Vec::new(),
            &mut rows,
        );
        rows
    }

    /// 返回指定链接配置。
    pub fn link(&self, link_id: ConnectionNodeId) -> Option<&ConnectionLinkConfig> {
        self.links.iter().find(|link| link.id == link_id)
    }

    /// 返回指定目录配置。
    pub fn directory(&self, directory_id: ConnectionNodeId) -> Option<&ConnectionDirectoryConfig> {
        self.directories
            .iter()
            .find(|directory| directory.id == directory_id)
    }

    /// 判断节点是否为目录。
    pub fn is_directory(&self, node_id: ConnectionNodeId) -> bool {
        self.directory(node_id).is_some()
    }

    /// 判断节点是否为 SSH 链接。
    pub fn is_link(&self, node_id: ConnectionNodeId) -> bool {
        self.link(node_id).is_some()
    }

    /// 返回目录或链接的父目录 ID。
    pub fn parent_id_for_node(&self, node_id: ConnectionNodeId) -> Option<ConnectionNodeId> {
        self.node_parent_id(node_id)
    }

    /// 判断目录是否没有任何直接子目录和链接。
    pub fn is_directory_empty(&self, directory_id: ConnectionNodeId) -> bool {
        !self
            .directories
            .iter()
            .any(|directory| directory.parent_id == Some(directory_id))
            && !self
                .links
                .iter()
                .any(|link| link.parent_id == Some(directory_id))
    }

    /// 保存或更新用户确认过的主机指纹。
    pub fn trust_host_key(&mut self, host: &str, port: u16, fingerprint: &str) {
        let host = normalized_required_text(host);
        let fingerprint = normalized_required_text(fingerprint);
        if let Some(existing) = self
            .trusted_hosts
            .iter_mut()
            .find(|trusted| trusted.host == host && trusted.port == port)
        {
            existing.fingerprint = fingerprint;
            return;
        }

        self.trusted_hosts.push(TrustedHostKeyConfig {
            host,
            port,
            fingerprint,
        });
    }

    /// 查询指定主机端口已经保存的可信指纹。
    pub fn trusted_fingerprint(&self, host: &str, port: u16) -> Option<&str> {
        let normalized_host = host.trim();
        self.trusted_hosts
            .iter()
            .find(|trusted| trusted.host == normalized_host && trusted.port == port)
            .map(|trusted| trusted.fingerprint.as_str())
    }

    /// 根据选中节点推导新节点父目录的内部实现。
    fn parent_for_new_node(
        &self,
        selected_id: Option<ConnectionNodeId>,
    ) -> Option<ConnectionNodeId> {
        let selected_id = selected_id?;
        if self.is_directory(selected_id) {
            Some(selected_id)
        } else {
            self.link(selected_id).and_then(|link| link.parent_id)
        }
    }

    /// 分配一个新的节点 ID，并推进下一个 ID 游标。
    fn take_next_id(&mut self) -> ConnectionNodeId {
        let id = self.next_id.max(default_next_connection_id());
        self.next_id = id + 1;
        id
    }

    /// 返回可变目录配置。
    fn directory_mut(
        &mut self,
        directory_id: ConnectionNodeId,
    ) -> Option<&mut ConnectionDirectoryConfig> {
        self.directories
            .iter_mut()
            .find(|directory| directory.id == directory_id)
    }

    /// 校验父节点必须为空或已存在目录。
    fn validate_parent_directory(
        &self,
        parent_id: Option<ConnectionNodeId>,
    ) -> Result<(), ConnectionValidationError> {
        if parent_id.is_some_and(|id| self.directory(id).is_none()) {
            return Err(ConnectionValidationError::ParentNotFound);
        }
        Ok(())
    }

    /// 校验同一目录下的目录和链接名称不能重复。
    fn validate_sibling_name_available(
        &self,
        parent_id: Option<ConnectionNodeId>,
        name: &str,
        ignored_id: Option<ConnectionNodeId>,
    ) -> Result<(), ConnectionValidationError> {
        let normalized_name = name.trim();
        let directory_conflict = self.directories.iter().any(|directory| {
            directory.parent_id == parent_id
                && Some(directory.id) != ignored_id
                && directory.name == normalized_name
        });
        let link_conflict = self.links.iter().any(|link| {
            link.parent_id == parent_id
                && Some(link.id) != ignored_id
                && link.name == normalized_name
        });
        if directory_conflict || link_conflict {
            Err(ConnectionValidationError::DuplicateName)
        } else {
            Ok(())
        }
    }

    /// 构建父目录到直接子节点的索引，保留用户创建顺序。
    fn child_index(&self) -> BTreeMap<Option<ConnectionNodeId>, Vec<ConnectionChildRef>> {
        let mut index: BTreeMap<Option<ConnectionNodeId>, Vec<ConnectionChildRef>> =
            BTreeMap::new();
        for directory in &self.directories {
            index
                .entry(directory.parent_id)
                .or_default()
                .push(ConnectionChildRef::Directory(directory.id));
        }
        for link in &self.links {
            index
                .entry(link.parent_id)
                .or_default()
                .push(ConnectionChildRef::Link(link.id));
        }
        index
    }

    /// 收集过滤命中的节点及其祖先节点。
    fn filtered_node_ids(&self, query: &str) -> BTreeSet<ConnectionNodeId> {
        let mut ids = BTreeSet::new();
        for directory in &self.directories {
            if directory.name.to_ascii_lowercase().contains(query) {
                self.insert_node_with_ancestors(directory.id, &mut ids);
            }
        }
        for link in &self.links {
            if link.matches_query(query) {
                self.insert_node_with_ancestors(link.id, &mut ids);
            }
        }
        ids
    }

    /// 将节点自身和所有祖先目录加入可见集合。
    fn insert_node_with_ancestors(
        &self,
        node_id: ConnectionNodeId,
        ids: &mut BTreeSet<ConnectionNodeId>,
    ) {
        ids.insert(node_id);
        let mut parent_id = self.node_parent_id(node_id);
        while let Some(current_parent_id) = parent_id {
            if !ids.insert(current_parent_id) {
                break;
            }
            parent_id = self
                .directory(current_parent_id)
                .and_then(|directory| directory.parent_id);
        }
    }

    /// 返回目录或链接的父目录 ID。
    fn node_parent_id(&self, node_id: ConnectionNodeId) -> Option<ConnectionNodeId> {
        self.directory(node_id)
            .and_then(|directory| directory.parent_id)
            .or_else(|| self.link(node_id).and_then(|link| link.parent_id))
    }

    /// 深度优先收集当前父目录下的可见行。
    #[allow(clippy::too_many_arguments)]
    fn collect_visible_rows(
        &self,
        parent_id: Option<ConnectionNodeId>,
        depth: usize,
        child_index: &BTreeMap<Option<ConnectionNodeId>, Vec<ConnectionChildRef>>,
        visible_ids: &BTreeSet<ConnectionNodeId>,
        is_filtering: bool,
        selected_id: Option<ConnectionNodeId>,
        ancestor_continuation_levels: &mut Vec<usize>,
        rows: &mut Vec<ConnectionTreeRow>,
    ) {
        let children = child_index.get(&parent_id).cloned().unwrap_or_default();
        let visible_children = children
            .into_iter()
            .filter(|child| !is_filtering || visible_ids.contains(&child.id()))
            .collect::<Vec<_>>();
        let visible_len = visible_children.len();

        for (index, child) in visible_children.into_iter().enumerate() {
            let has_next_sibling = index + 1 < visible_len;
            match child {
                ConnectionChildRef::Directory(directory_id) => {
                    let Some(directory) = self.directory(directory_id) else {
                        continue;
                    };
                    let has_children = child_index
                        .get(&Some(directory_id))
                        .is_some_and(|children| !children.is_empty());
                    rows.push(ConnectionTreeRow {
                        id: directory.id,
                        parent_id: directory.parent_id,
                        depth,
                        label: directory.name.clone(),
                        kind: ConnectionTreeRowKind::Directory,
                        expanded: directory.expanded || is_filtering,
                        has_children,
                        is_selected: selected_id == Some(directory.id),
                        has_next_sibling,
                        ancestor_continuation_levels: ancestor_continuation_levels.clone(),
                    });
                    if has_children && (directory.expanded || is_filtering) {
                        if has_next_sibling {
                            ancestor_continuation_levels.push(depth);
                        }
                        self.collect_visible_rows(
                            Some(directory_id),
                            depth + 1,
                            child_index,
                            visible_ids,
                            is_filtering,
                            selected_id,
                            ancestor_continuation_levels,
                            rows,
                        );
                        if has_next_sibling {
                            ancestor_continuation_levels.pop();
                        }
                    }
                }
                ConnectionChildRef::Link(link_id) => {
                    let Some(link) = self.link(link_id) else {
                        continue;
                    };
                    rows.push(ConnectionTreeRow {
                        id: link.id,
                        parent_id: link.parent_id,
                        depth,
                        label: link.name.clone(),
                        kind: ConnectionTreeRowKind::SshLink,
                        expanded: false,
                        has_children: false,
                        is_selected: selected_id == Some(link.id),
                        has_next_sibling,
                        ancestor_continuation_levels: ancestor_continuation_levels.clone(),
                    });
                }
            }
        }
    }
}

/// 单个链接目录配置。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ConnectionDirectoryConfig {
    /// 目录节点 ID。
    pub id: ConnectionNodeId,
    /// 父目录 ID；为空表示根层级。
    pub parent_id: Option<ConnectionNodeId>,
    /// 目录展示名称。
    pub name: String,
    /// 是否展开；过滤时 UI 会临时展开命中路径。
    #[serde(default = "default_expanded")]
    pub expanded: bool,
}

/// 单个 SSH 链接配置。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ConnectionLinkConfig {
    /// 链接节点 ID。
    pub id: ConnectionNodeId,
    /// 父目录 ID；为空表示根层级。
    pub parent_id: Option<ConnectionNodeId>,
    /// 链接展示名称。
    pub name: String,
    /// SSH 连接参数。
    pub ssh: SshLinkConfig,
}

impl ConnectionLinkConfig {
    /// 返回状态栏和终端标题可展示的 SSH 地址。
    pub fn address_label(&self) -> String {
        format!("{}@{}:{}", self.ssh.username, self.ssh.host, self.ssh.port)
    }

    /// 判断链接是否匹配过滤关键字。
    fn matches_query(&self, query: &str) -> bool {
        self.name.to_ascii_lowercase().contains(query)
            || self.ssh.host.to_ascii_lowercase().contains(query)
            || self.ssh.username.to_ascii_lowercase().contains(query)
    }
}

/// SSH 链接参数；按当前产品选择，密码和私钥口令也会持久化到配置文件。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SshLinkConfig {
    /// 远程主机名或 IP。
    pub host: String,
    /// SSH 端口。
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    /// 登录用户名。
    pub username: String,
    /// 密码鉴权字段；为空时跳过密码登录。
    #[serde(default)]
    pub password: String,
    /// 私钥文件路径；为空时跳过私钥登录。
    #[serde(default)]
    pub private_key_path: Option<String>,
    /// 私钥口令；为空时按无口令私钥处理。
    #[serde(default)]
    pub private_key_passphrase: Option<String>,
}

impl Default for SshLinkConfig {
    /// 构造新增 SSH 链接表单使用的默认值。
    fn default() -> Self {
        Self {
            host: String::new(),
            port: DEFAULT_SSH_PORT,
            username: String::new(),
            password: String::new(),
            private_key_path: None,
            private_key_passphrase: None,
        }
    }
}

impl SshLinkConfig {
    /// 归一化并校验 SSH 配置，确保保存前已经满足第一版连接条件。
    pub fn normalized_for_save(mut self) -> Result<Self, ConnectionValidationError> {
        self.host = validate_required_text(&self.host, ConnectionValidationError::MissingHost)?;
        self.username =
            validate_required_text(&self.username, ConnectionValidationError::MissingUsername)?;
        if self.port == 0 {
            return Err(ConnectionValidationError::InvalidPort);
        }
        self.private_key_path = normalized_optional_text(self.private_key_path.take());
        self.private_key_passphrase =
            normalized_optional_secret_text(self.private_key_passphrase.take());
        if self.password.is_empty() && self.private_key_path.is_none() {
            return Err(ConnectionValidationError::MissingCredential);
        }
        Ok(self)
    }
}

/// 用户确认可信的 SSH 主机指纹。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TrustedHostKeyConfig {
    /// 远程主机名或 IP。
    pub host: String,
    /// SSH 端口。
    pub port: u16,
    /// 格式化后的 SHA256 指纹，例如 `SHA256:xxxx`。
    pub fingerprint: String,
}

/// 链接目录树的一行可见节点。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectionTreeRow {
    /// 节点 ID。
    pub id: ConnectionNodeId,
    /// 父目录 ID。
    pub parent_id: Option<ConnectionNodeId>,
    /// 当前行缩进层级。
    pub depth: usize,
    /// 当前行展示名称。
    pub label: String,
    /// 当前行节点类型。
    pub kind: ConnectionTreeRowKind,
    /// 目录是否展开；链接始终为 false。
    pub expanded: bool,
    /// 当前节点是否存在子节点。
    pub has_children: bool,
    /// 当前节点是否为 UI 选中状态。
    pub is_selected: bool,
    /// 当前节点之后是否还有同级节点，用于绘制目录连线。
    pub has_next_sibling: bool,
    /// 需要向下延伸竖线的祖先层级。
    pub ancestor_continuation_levels: Vec<usize>,
}

/// 链接目录树节点类型。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionTreeRowKind {
    /// 可展开目录节点。
    Directory,
    /// SSH 链接叶子节点。
    SshLink,
}

/// 删除连接节点后的节点类型，用于应用层展示差异化提示。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionDeletedNodeKind {
    /// 被删除的是目录。
    Directory,
    /// 被删除的是 SSH 链接。
    SshLink,
}

/// 创建或保存连接配置时的校验错误。
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ConnectionValidationError {
    /// 节点名称为空。
    #[error("名称不能为空")]
    MissingName,
    /// SSH 主机为空。
    #[error("主机不能为空")]
    MissingHost,
    /// SSH 用户名为空。
    #[error("用户名不能为空")]
    MissingUsername,
    /// SSH 端口非法。
    #[error("端口必须在 1 到 65535 之间")]
    InvalidPort,
    /// 未填写任何支持的鉴权凭据。
    #[error("请填写密码或私钥路径")]
    MissingCredential,
    /// 父目录不存在。
    #[error("父目录不存在")]
    ParentNotFound,
    /// 待编辑或删除的节点不存在。
    #[error("连接节点不存在")]
    NodeNotFound,
    /// 非空目录不能删除。
    #[error("目录不为空，不能删除")]
    DirectoryNotEmpty,
    /// 同级目录或链接重名。
    #[error("同一目录下已存在同名目录或链接")]
    DuplicateName,
}

/// 子节点引用，避免构建可见行时复制完整配置。
#[derive(Clone, Copy, Debug)]
enum ConnectionChildRef {
    /// 目录子节点。
    Directory(ConnectionNodeId),
    /// SSH 链接子节点。
    Link(ConnectionNodeId),
}

impl ConnectionChildRef {
    /// 返回子节点 ID。
    fn id(self) -> ConnectionNodeId {
        match self {
            Self::Directory(id) | Self::Link(id) => id,
        }
    }
}

/// 默认下一个节点 ID，从 1 开始便于 `0` 保留给无效值。
fn default_next_connection_id() -> ConnectionNodeId {
    1
}

/// 默认目录展开状态；新建目录展开以便立即展示其子项。
fn default_expanded() -> bool {
    true
}

/// 返回 SSH 默认端口。
fn default_ssh_port() -> u16 {
    DEFAULT_SSH_PORT
}

/// 校验节点名称并返回去除首尾空白后的文本。
fn validate_node_name(name: &str) -> Result<String, ConnectionValidationError> {
    validate_required_text(name, ConnectionValidationError::MissingName)
}

/// 校验必填文本字段。
fn validate_required_text(
    value: &str,
    error: ConnectionValidationError,
) -> Result<String, ConnectionValidationError> {
    let normalized = normalized_required_text(value);
    if normalized.is_empty() {
        Err(error)
    } else {
        Ok(normalized)
    }
}

/// 归一化必填文本字段，统一去除首尾空白。
fn normalized_required_text(value: &str) -> String {
    value.trim().to_string()
}

/// 归一化可选文本字段，空字符串保存为 `None`。
fn normalized_optional_text(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim().to_string();
        (!value.is_empty()).then_some(value)
    })
}

/// 归一化可选敏感文本字段；只把真正空字符串保存为 `None`，避免改写用户凭据。
fn normalized_optional_secret_text(value: Option<String>) -> Option<String> {
    value.and_then(|value| (!value.is_empty()).then_some(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造含有两层目录和一个 SSH 链接的测试配置。
    fn sample_config() -> ConnectionConfig {
        let mut config = ConnectionConfig::default();
        let root = config.add_directory(None, "生产环境").unwrap();
        let app = config.add_directory(Some(root), "应用服务器").unwrap();
        config
            .add_ssh_link(
                Some(app),
                "app-01",
                SshLinkConfig {
                    host: "10.0.0.1".to_string(),
                    port: 22,
                    username: "deploy".to_string(),
                    password: "secret".to_string(),
                    private_key_path: None,
                    private_key_passphrase: None,
                },
            )
            .unwrap();
        config
    }

    /// 验证根目录和子目录创建时会分配稳定父子关系。
    #[test]
    fn add_directory_assigns_parent_and_unique_ids() {
        let mut config = ConnectionConfig::default();

        let root = config.add_directory(None, "根目录").unwrap();
        let child = config.add_directory(Some(root), "子目录").unwrap();

        assert_ne!(root, child);
        assert_eq!(config.directory(child).unwrap().parent_id, Some(root));
    }

    /// 验证新增链接会使用当前目录作为父目录，选中链接时使用链接的父目录。
    #[test]
    fn parent_for_new_link_uses_selected_directory_or_link_parent() {
        let config = sample_config();
        let directory_id = config
            .directories
            .iter()
            .find(|directory| directory.name == "应用服务器")
            .unwrap()
            .id;
        let link_id = config.links[0].id;

        assert_eq!(
            config.parent_for_new_link(Some(directory_id)),
            Some(directory_id)
        );
        assert_eq!(
            config.parent_for_new_link(Some(link_id)),
            Some(directory_id)
        );
        assert_eq!(config.parent_for_new_link(None), None);
    }

    /// 验证新增目录只在选中目录时创建子目录，选中链接时回到根层级。
    #[test]
    fn parent_for_new_directory_only_uses_selected_directory() {
        let config = sample_config();
        let directory_id = config
            .directories
            .iter()
            .find(|directory| directory.name == "应用服务器")
            .unwrap()
            .id;
        let link_id = config.links[0].id;

        assert_eq!(
            config.parent_for_new_directory(Some(directory_id)),
            Some(directory_id)
        );
        assert_eq!(config.parent_for_new_directory(Some(link_id)), None);
        assert_eq!(config.parent_for_new_directory(None), None);
    }

    /// 验证过滤结果会保留命中链接的祖先目录。
    #[test]
    fn visible_rows_filter_keeps_ancestors() {
        let config = sample_config();

        let rows = config.visible_rows("app-01", None);
        let labels = rows
            .iter()
            .map(|row| row.label.as_str())
            .collect::<Vec<_>>();

        assert_eq!(labels, vec!["生产环境", "应用服务器", "app-01"]);
    }

    /// 验证同级目录和链接不能重名。
    #[test]
    fn duplicate_sibling_names_are_rejected() {
        let mut config = ConnectionConfig::default();
        config.add_directory(None, "生产环境").unwrap();

        let error = config.add_directory(None, "生产环境").unwrap_err();

        assert_eq!(error, ConnectionValidationError::DuplicateName);
    }

    /// 验证编辑目录时仍会拦截同级重名。
    #[test]
    fn update_directory_rejects_duplicate_sibling_name() {
        let mut config = ConnectionConfig::default();
        let first = config.add_directory(None, "生产环境").unwrap();
        let second = config.add_directory(None, "测试环境").unwrap();

        let error = config.update_directory(second, "生产环境").unwrap_err();

        assert_eq!(error, ConnectionValidationError::DuplicateName);
        assert_eq!(config.directory(first).unwrap().name, "生产环境");
        assert_eq!(config.directory(second).unwrap().name, "测试环境");
    }

    /// 验证非空目录不能删除，避免右键删除误删整棵子树。
    #[test]
    fn delete_directory_rejects_non_empty_directory() {
        let mut config = sample_config();
        let root = config
            .directories
            .iter()
            .find(|directory| directory.name == "生产环境")
            .unwrap()
            .id;

        let error = config.delete_node(root).unwrap_err();

        assert_eq!(error, ConnectionValidationError::DirectoryNotEmpty);
        assert!(config.directory(root).is_some());
    }

    /// 验证 SSH 链接可被删除，目录为空后也可以继续删除目录。
    #[test]
    fn delete_link_then_empty_directory_succeeds() {
        let mut config = sample_config();
        let link_id = config.links[0].id;
        let app_dir = config.links[0].parent_id.unwrap();

        assert_eq!(
            config.delete_node(link_id).unwrap(),
            ConnectionDeletedNodeKind::SshLink
        );
        assert!(config.link(link_id).is_none());
        assert_eq!(
            config.delete_node(app_dir).unwrap(),
            ConnectionDeletedNodeKind::Directory
        );
    }

    /// 验证 SSH 配置必须包含端口、主机、用户名和至少一种凭据。
    #[test]
    fn ssh_link_validation_rejects_missing_credentials() {
        let mut config = ConnectionConfig::default();

        let error = config
            .add_ssh_link(
                None,
                "app-01",
                SshLinkConfig {
                    host: "10.0.0.1".to_string(),
                    port: 22,
                    username: "deploy".to_string(),
                    password: String::new(),
                    private_key_path: None,
                    private_key_passphrase: None,
                },
            )
            .unwrap_err();

        assert_eq!(error, ConnectionValidationError::MissingCredential);
    }

    /// 验证 SSH 密码和私钥口令不会被裁剪，避免静默改写用户真实凭据。
    #[test]
    fn ssh_link_validation_preserves_secret_whitespace() {
        let ssh = SshLinkConfig {
            host: " 10.0.0.1 ".to_string(),
            port: 22,
            username: " deploy ".to_string(),
            password: " secret ".to_string(),
            private_key_path: Some(" ~/.ssh/id_ed25519 ".to_string()),
            private_key_passphrase: Some(" phrase ".to_string()),
        }
        .normalized_for_save()
        .unwrap();

        assert_eq!(ssh.host, "10.0.0.1");
        assert_eq!(ssh.username, "deploy");
        assert_eq!(ssh.password, " secret ");
        assert_eq!(ssh.private_key_path.as_deref(), Some("~/.ssh/id_ed25519"));
        assert_eq!(ssh.private_key_passphrase.as_deref(), Some(" phrase "));
    }
}
