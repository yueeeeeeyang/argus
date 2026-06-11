//! 文件职责：提供应用状态单元测试使用的占位来源树和日志数据。
//! 创建日期：2026-06-11
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：隔离测试 fixture，避免正式应用状态文件混入大量样例构造逻辑。

#![cfg(test)]

use std::path::PathBuf;

use super::LogLine;
use crate::loader::archive::ArchiveFormat;
use crate::loader::{SourceKind, SourceLocation, SourceMetadata, SourceRegistry, SourceTreeNode};

/// 构造测试样例来源树，便于验证过滤和展开状态，不参与正式启动界面。
pub(super) fn placeholder_source_registry() -> SourceRegistry {
    let mut registry = SourceRegistry::new();
    let logs_id = registry.allocate_id();
    registry.insert_node(SourceTreeNode {
        id: logs_id,
        parent_id: None,
        depth: 0,
        label: "logs".into(),
        kind: SourceKind::Directory,
        location: SourceLocation::LocalPath(PathBuf::from("logs")),
        metadata: SourceMetadata {
            size: None,
            children_loaded: true,
            is_loading: false,
            message: None,
        },
        selected: false,
        expanded: true,
    });

    let app_id = registry.allocate_id();
    registry.insert_node(SourceTreeNode {
        id: app_id,
        parent_id: Some(logs_id),
        depth: 1,
        label: "app.log".into(),
        kind: SourceKind::LogFile,
        location: SourceLocation::LocalPath(PathBuf::from("logs/app.log")),
        metadata: SourceMetadata {
            size: Some(7494),
            children_loaded: true,
            is_loading: false,
            message: None,
        },
        selected: true,
        expanded: false,
    });

    let error_id = registry.allocate_id();
    registry.insert_node(SourceTreeNode {
        id: error_id,
        parent_id: Some(logs_id),
        depth: 1,
        label: "error.log".into(),
        kind: SourceKind::LogFile,
        location: SourceLocation::LocalPath(PathBuf::from("logs/error.log")),
        metadata: SourceMetadata {
            size: Some(2048),
            children_loaded: true,
            is_loading: false,
            message: None,
        },
        selected: false,
        expanded: false,
    });

    let archive_id = registry.allocate_id();
    registry.insert_node(SourceTreeNode {
        id: archive_id,
        parent_id: None,
        depth: 0,
        label: "archive.zip".into(),
        kind: SourceKind::Archive(ArchiveFormat::Zip),
        location: SourceLocation::LocalPath(PathBuf::from("archive.zip")),
        metadata: SourceMetadata {
            size: Some(4096),
            children_loaded: true,
            is_loading: false,
            message: None,
        },
        selected: false,
        expanded: true,
    });

    let nested_id = registry.allocate_id();
    registry.insert_node(SourceTreeNode {
        id: nested_id,
        parent_id: Some(archive_id),
        depth: 1,
        label: "nested.log".into(),
        kind: SourceKind::ArchiveFile,
        location: SourceLocation::ArchiveEntry {
            archive_path: PathBuf::from("archive.zip"),
            root_format: ArchiveFormat::Zip,
            container_entries: Vec::new(),
            entry_path: "nested.log".into(),
            format: ArchiveFormat::Zip,
            archive_depth: 0,
        },
        metadata: SourceMetadata {
            size: Some(1024),
            children_loaded: true,
            is_loading: false,
            message: None,
        },
        selected: false,
        expanded: false,
    });

    registry.rebuild_all_indices();
    registry
}

/// 构造日志内容占位数据，覆盖常见日志等级视觉状态。
pub(super) fn placeholder_logs() -> Vec<LogLine> {
    vec![
        LogLine {
            number: 1,
            level: "INFO".into(),
            message: "2024-01-15 10:23:45 [INFO] Application started".into(),
        },
        LogLine {
            number: 2,
            level: "DEBUG".into(),
            message: "2024-01-15 10:23:46 [DEBUG] Loading config...".into(),
        },
        LogLine {
            number: 3,
            level: "INFO".into(),
            message: "2024-01-15 10:23:46 [INFO] Config loaded".into(),
        },
        LogLine {
            number: 4,
            level: "WARN".into(),
            message: "2024-01-15 10:23:47 [WARN] Deprecated API usage".into(),
        },
        LogLine {
            number: 5,
            level: "ERROR".into(),
            message: "2024-01-15 10:23:48 [ERROR] Failed to connect".into(),
        },
        LogLine {
            number: 6,
            level: "INFO".into(),
            message: "2024-01-15 10:23:48 [INFO] Retrying connection...".into(),
        },
        LogLine {
            number: 7,
            level: "DEBUG".into(),
            message: "2024-01-15 10:23:49 [DEBUG] Connection established".into(),
        },
        LogLine {
            number: 8,
            level: "INFO".into(),
            message: "2024-01-15 10:23:50 [INFO] Request received".into(),
        },
    ]
}
