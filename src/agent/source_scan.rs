//! 文件职责：基于当前来源注册表和工作目录直接固化 Agent 会话范围快照。
//! 创建日期：2026-07-15
//! 修改日期：2026-09-14
//! 作者：Argus 开发团队
//! 主要功能：从已完整初始化的来源树生成工作区清单快照，并统计日志类型规则命中情况。

use std::path::Path;

use crate::agent::session::{AgentScopeSelection, SourceScopeSnapshot};
use crate::config::{AiConfig, LogNameMatcherMode, LogNameMatcherTarget, LogTypeProfile};
use crate::loader::{SourceId, SourceRegistry};

/// Agent 会话范围固化结果。
#[derive(Debug)]
pub(crate) struct AgentSourcePreparation {
    /// 已完成日志类型匹配的工作区清单快照。
    pub scope: SourceScopeSnapshot,
    /// 当前所有已启用日志类型及其逐规则命中统计，只供会话窗口解释匹配结果。
    pub match_summaries: Vec<AgentLogProfileMatchSummary>,
}

/// 一个日志类型在当前来源范围内的匹配统计。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AgentLogProfileMatchSummary {
    /// 用户配置的日志类型名称。
    pub profile_name: String,
    /// 配置优先级；多个类型重叠时用于解释最终采用结果。
    pub priority: u16,
    /// 至少命中该类型任意规则的文件数，不考虑其它类型的优先级。
    pub matched_file_count: usize,
    /// 经过跨类型优先级选择后，最终采用该日志类型的文件数。
    pub selected_file_count: usize,
    /// 按配置顺序保存的逐规则命中结果。
    pub rules: Vec<AgentLogRuleMatchSummary>,
}

/// 一条日志名称匹配规则在当前来源范围内的命中统计。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AgentLogRuleMatchSummary {
    /// 规则作用于末级文件名还是来源内相对路径。
    pub target: LogNameMatcherTarget,
    /// 规则使用的精确、前缀、后缀、包含或正则算法。
    pub mode: LogNameMatcherMode,
    /// 会话开始时固化的规则模式，仅用于界面解释。
    pub pattern: String,
    /// 是否区分大小写。
    pub case_sensitive: bool,
    /// 当前来源范围内命中该规则的日志文件数。
    pub matched_file_count: usize,
}

/// 按当前选中节点解析分析根并固化工作区清单快照。
///
/// 参数说明：
/// - `registry`：加载时已一次性完整初始化的来源树；
/// - `selected_id`：当前选中节点，用于解析所属顶层来源根；
/// - `config`：已经规范化和校验的 AI 配置，包含日志类型名称匹配规则；
/// - `workspace_root`：当前日志工作目录根；
/// - `preferred_encoding`：用户选择的兜底解码编码。
///
/// 返回值：至少存在一个日志候选时返回范围快照和规则命中统计。
pub(crate) fn prepare_agent_source_scope(
    registry: &SourceRegistry,
    selected_id: Option<SourceId>,
    config: AiConfig,
    workspace_root: &Path,
    preferred_encoding: &str,
) -> Result<AgentSourcePreparation, String> {
    prepare_agent_source_scope_for_selection(
        registry,
        AgentScopeSelection::SelectedRoot(selected_id),
        config,
        workspace_root,
        preferred_encoding,
    )
}

/// 按显式单根或全部根范围固化工作区清单快照。
///
/// 来源树在日志加载时已经一次性完整初始化，快照构建只读取既有节点和工作目录路径，
/// 不再触发任何文件系统扫描。
pub(crate) fn prepare_agent_source_scope_for_selection(
    registry: &SourceRegistry,
    selection: AgentScopeSelection,
    config: AiConfig,
    workspace_root: &Path,
    preferred_encoding: &str,
) -> Result<AgentSourcePreparation, String> {
    let scope = SourceScopeSnapshot::from_registry_selection(
        registry,
        selection,
        &config,
        workspace_root,
        preferred_encoding,
    )?;
    let match_summaries = build_match_summaries(&config.log_profiles, &scope);

    Ok(AgentSourcePreparation {
        scope,
        match_summaries,
    })
}

/// 统计每个已启用日志类型及其每条规则在当前来源范围内的命中文件数。
///
/// 统计采用原始规则命中语义：同一个文件可以同时计入多条规则；`selected_file_count`
/// 另行记录优先级决胜后的最终类型，避免规则重叠时把原始命中误解为实际采用。
fn build_match_summaries(
    profiles: &[LogTypeProfile],
    scope: &SourceScopeSnapshot,
) -> Vec<AgentLogProfileMatchSummary> {
    profiles
        .iter()
        .filter(|profile| profile.enabled && profile.validate().is_ok())
        .map(|profile| {
            let mut rule_match_counts = vec![0_usize; profile.matchers.len()];
            let mut matched_file_count = 0_usize;
            // 每个文件只遍历一次当前类型的规则，同时累计规则明细和类型 OR 命中数，避免为汇总重复匹配。
            for source in scope.sources.iter() {
                let mut is_profile_matched = false;
                for (rule_index, matcher) in profile.matchers.iter().enumerate() {
                    if matcher.is_match(&source.file_name, &source.profile_match_path) {
                        rule_match_counts[rule_index] += 1;
                        is_profile_matched = true;
                    }
                }
                if is_profile_matched {
                    matched_file_count += 1;
                }
            }
            let rules = profile
                .matchers
                .iter()
                .zip(rule_match_counts)
                .map(|(matcher, matched_file_count)| AgentLogRuleMatchSummary {
                    target: matcher.target,
                    mode: matcher.mode,
                    pattern: matcher.pattern.clone(),
                    case_sensitive: matcher.case_sensitive,
                    matched_file_count,
                })
                .collect::<Vec<_>>();
            let selected_file_count = scope
                .sources
                .iter()
                .filter(|source| source.profile_id.as_deref() == Some(profile.profile_id.as_str()))
                .count();
            AgentLogProfileMatchSummary {
                profile_name: profile.name.clone(),
                priority: profile.priority,
                matched_file_count,
                selected_file_count,
                rules,
            }
        })
        .collect()
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

        let preparation =
            prepare_agent_source_scope(&registry, None, config, &workspace_path, "UTF-8")
                .expect("完整来源树应直接生成快照");

        assert_eq!(preparation.scope.sources.len(), 1);
        assert_eq!(
            preparation.scope.sources[0].profile_id.as_deref(),
            Some(profile_id.as_str())
        );
        assert_eq!(preparation.scope.profiles.len(), 1);
        assert_eq!(preparation.match_summaries.len(), 1);
        assert_eq!(preparation.match_summaries[0].matched_file_count, 1);
        assert_eq!(preparation.match_summaries[0].selected_file_count, 1);
        assert_eq!(preparation.match_summaries[0].rules.len(), 1);
        assert_eq!(
            preparation.match_summaries[0].rules[0].matched_file_count,
            1
        );
    }

    /// 验证每条规则独立计数，未命中规则保留零值，且同一文件可命中多条规则。
    #[test]
    fn source_scan_reports_each_matcher_hit_count() {
        let workspace = temporary_test_dir("source-scan-match-count");
        let workspace_path = workspace.path().to_path_buf();

        let mut registry = SourceRegistry::new();
        insert_loaded_tree(&mut registry, "logs", &workspace_path);
        insert_log_file(
            &mut registry,
            "logs",
            &workspace_path,
            "memory_20260715.log",
        );
        insert_log_file(&mut registry, "logs", &workspace_path, "application.log");
        registry.rebuild_all_indices();

        let mut config = AiConfig::default();
        config.log_profiles.push(LogTypeProfile {
            profile_id: Uuid::new_v4().to_string(),
            enabled: true,
            name: "内存日志".to_string(),
            priority: 100,
            matchers: vec![
                LogNameMatcher {
                    target: LogNameMatcherTarget::FileName,
                    mode: LogNameMatcherMode::Prefix,
                    pattern: "memory_".to_string(),
                    case_sensitive: false,
                },
                LogNameMatcher {
                    target: LogNameMatcherTarget::FileName,
                    mode: LogNameMatcherMode::Suffix,
                    pattern: ".log".to_string(),
                    case_sensitive: false,
                },
                LogNameMatcher {
                    target: LogNameMatcherTarget::FileName,
                    mode: LogNameMatcherMode::Contains,
                    pattern: "not-present".to_string(),
                    case_sensitive: false,
                },
            ],
            description: "用于分析内存压力".to_string(),
        });

        let preparation =
            prepare_agent_source_scope(&registry, None, config, &workspace_path, "UTF-8")
                .expect("来源快照应构建成功");
        let summary = &preparation.match_summaries[0];

        assert_eq!(summary.matched_file_count, 2);
        assert_eq!(summary.selected_file_count, 2);
        assert_eq!(summary.rules[0].matched_file_count, 1);
        assert_eq!(summary.rules[1].matched_file_count, 2);
        assert_eq!(summary.rules[2].matched_file_count, 0);
    }

    /// 验证交互助手一次固化全部来源根，并为同名日志生成带根前缀的工作区路径。
    #[test]
    fn assistant_source_scan_covers_all_roots_and_disambiguates_paths() {
        let workspace = temporary_test_dir("assistant-source-scan-roots");
        let workspace_path = workspace.path().to_path_buf();

        let mut registry = SourceRegistry::new();
        insert_loaded_tree(&mut registry, "logs", &workspace_path);
        insert_log_file(&mut registry, "logs", &workspace_path, "application.log");
        insert_loaded_tree(&mut registry, "logs", &workspace_path);
        insert_log_file(&mut registry, "logs", &workspace_path, "application.log");
        registry.rebuild_all_indices();

        let profile_id = Uuid::new_v4().to_string();
        let config = AiConfig {
            log_profiles: vec![LogTypeProfile {
                profile_id: profile_id.clone(),
                enabled: true,
                name: "应用日志".to_string(),
                priority: 100,
                matchers: vec![LogNameMatcher {
                    target: LogNameMatcherTarget::RelativePath,
                    mode: LogNameMatcherMode::Exact,
                    pattern: "application.log".to_string(),
                    case_sensitive: false,
                }],
                description: "验证多根展示前缀不改变路径匹配".to_string(),
            }],
            ..AiConfig::default()
        };
        let preparation = prepare_agent_source_scope_for_selection(
            &registry,
            AgentScopeSelection::AllLoadedRoots,
            config,
            &workspace_path,
            "UTF-8",
        )
        .expect("助手应固化全部根来源");

        let paths = preparation
            .scope
            .sources
            .iter()
            .map(|source| source.workspace_path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(preparation.scope.sources.len(), 2);
        assert!(paths.contains(&"logs (1)/application.log"));
        assert!(paths.contains(&"logs (2)/application.log"));
        assert!(preparation.scope.sources.iter().all(|source| {
            source.profile_id.as_deref() == Some(profile_id.as_str())
                && source.profile_match_path == "application.log"
                && source
                    .absolute_path
                    .starts_with(preparation.scope.workspace_root.as_path())
        }));
        assert_eq!(preparation.match_summaries[0].matched_file_count, 2);
    }
}
