//! 文件职责：基于当前来源注册表和工作目录直接固化 Agent 会话范围快照。
//! 创建日期：2026-07-15
//! 修改日期：2026-10-28
//! 作者：Argus 开发团队
//! 主要功能：从已完整初始化的来源树生成全部来源的工作区清单快照。

use std::path::Path;

use crate::agent::session::SourceScopeSnapshot;
use crate::config::AiConfig;
use crate::loader::SourceRegistry;

/// 固化来源注册表中全部已加载根的工作区清单快照。
///
/// 参数说明：
/// - `registry`：加载时已一次性完整初始化的来源树；
/// - `config`：已经规范化和校验的 AI 配置，包含日志类型名称匹配规则；
/// - `workspace_root`：当前日志工作目录根；
/// - `preferred_encoding`：用户选择的兜底解码编码。
///
/// 返回值：至少存在一个日志候选时返回范围快照和错误原因。
pub(crate) fn prepare_agent_source_scope(
    registry: &SourceRegistry,
    config: AiConfig,
    workspace_root: &Path,
    preferred_encoding: &str,
) -> Result<SourceScopeSnapshot, String> {
    // 来源树在日志加载时已经一次性完整初始化，快照构建只读取既有节点和工作目录路径。
    SourceScopeSnapshot::from_all_loaded_roots(
        registry,
        &config,
        workspace_root,
        preferred_encoding,
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use uuid::Uuid;

    use super::*;
    use crate::config::paths::temporary_test_dir;
    use crate::config::{LogNameMatcher, LogNameMatcherMode, LogNameMatcherTarget, LogTypeProfile};
    use crate::loader::{SourceKind, SourceLocation, SourceMetadata, SourceTreeNode};

    /// 在临时工作目录下插入一个完整加载的来源树。
    fn insert_loaded_tree(registry: &mut SourceRegistry, root_label: &str, workspace: &Path) {
        let root_id = registry.allocate_id();
        registry.insert_node(SourceTreeNode {
            id: root_id,
            parent_id: None,
            depth: 0,
            label: root_label.to_string(),
            kind: SourceKind::Directory,
            location: SourceLocation::LocalPath(workspace.join(root_label)),
            metadata: SourceMetadata {
                children_loaded: true,
                ..SourceMetadata::default()
            },
            selected: false,
            expanded: false,
        });
    }

    /// 在指定根下插入一个日志文件节点。
    fn insert_log_file(
        registry: &mut SourceRegistry,
        root_label: &str,
        workspace: &Path,
        file_name: &str,
    ) {
        let root_id = *registry.root_ids().last().expect("应已有来源根");
        let file_id = registry.allocate_id();
        registry.insert_node(SourceTreeNode {
            id: file_id,
            parent_id: Some(root_id),
            depth: 1,
            label: file_name.to_string(),
            kind: SourceKind::LogFile,
            location: SourceLocation::LocalPath(workspace.join(root_label).join(file_name)),
            metadata: SourceMetadata::default(),
            selected: false,
            expanded: false,
        });
    }

    /// 验证快照直接读取完整来源树并按文件名匹配日志类型说明。
    #[test]
    fn source_snapshot_builds_from_complete_tree_and_matches_profile() {
        let workspace = temporary_test_dir("source-scan-complete");
        let workspace_path = workspace.path().to_path_buf();
        fs::create_dir_all(workspace_path.join("logs")).expect("应创建来源目录");
        fs::write(
            workspace_path.join("logs/application.log"),
            "startup failed",
        )
        .expect("应写入测试日志");

        let mut registry = SourceRegistry::new();
        insert_loaded_tree(&mut registry, "logs", &workspace_path);
        insert_log_file(&mut registry, "logs", &workspace_path, "application.log");
        registry.rebuild_all_indices();

        let profile_id = Uuid::new_v4().to_string();
        let mut config = AiConfig::default();
        config.log_profiles.push(LogTypeProfile {
            profile_id: profile_id.clone(),
            enabled: true,
            name: "应用日志".to_string(),
            priority: 100,
            matchers: vec![LogNameMatcher {
                target: LogNameMatcherTarget::FileName,
                mode: LogNameMatcherMode::Suffix,
                pattern: ".log".to_string(),
                case_sensitive: false,
            }],
            description: "用于分析应用启动和运行异常".to_string(),
        });

        let scope = prepare_agent_source_scope(&registry, config, &workspace_path, "UTF-8")
            .expect("完整来源树应直接生成快照");

        assert_eq!(scope.sources.len(), 1);
        assert_eq!(
            scope.sources[0].profile_id.as_deref(),
            Some(profile_id.as_str())
        );
        assert_eq!(scope.profiles.len(), 1);
    }
}
