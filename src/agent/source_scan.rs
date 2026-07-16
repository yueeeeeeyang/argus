//! 文件职责：在 AI 会话启动前完整补齐分析范围内的来源树并生成不可变范围快照。
//! 创建日期：2026-07-15
//! 修改日期：2026-07-16
//! 作者：Argus 开发团队
//! 主要功能：复用现有来源加载器递归扫描未展开目录和归档，匹配日志类型说明，并返回可安全回填 UI 的注册表副本。

use std::collections::BTreeSet;

use crate::agent::session::SourceScopeSnapshot;
use crate::config::{
    AiConfig, LoaderConfig, LogNameMatcherMode, LogNameMatcherTarget, LogTypeProfile,
};
use crate::loader::archive::ArchivePasswordStore;
use crate::loader::{LoadReport, LogSourceLoader, SourceId, SourceRegistry};

/// AI 来源扫描完成后的不可变会话范围及补齐后的来源注册表。
#[derive(Debug)]
pub(crate) struct AgentSourcePreparation {
    /// 完整扫描后的来源树副本；回填主应用后可继续支持报告证据跳转。
    pub registry: SourceRegistry,
    /// 已完成日志类型匹配的 Agent 会话范围。
    pub scope: SourceScopeSnapshot,
    /// 扫描过程中可容忍的局部读取警告；不包含日志正文。
    pub warnings: Vec<String>,
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

/// 完整扫描选定来源根并生成 Agent 会话快照。
///
/// 参数说明：
/// - `registry`：点击开始分析时复制的来源树，后台任务只修改该副本；
/// - `selected_id`：当前选中节点，用于解析所属顶层来源根；
/// - `config`：已经规范化和校验的 AI 配置，包含日志类型名称匹配规则；
/// - `default_encoding`：日志工具默认使用的字符编码；
/// - `loader_config`：目录、符号链接和归档深度边界；
/// - `archive_passwords`：当前进程已经获得授权的归档密码快照。
///
/// 返回值：扫描成功且至少发现一个日志候选时返回完整注册表和范围快照。
pub(crate) fn prepare_agent_source_scope(
    mut registry: SourceRegistry,
    selected_id: Option<SourceId>,
    config: AiConfig,
    default_encoding: String,
    loader_config: LoaderConfig,
    archive_passwords: ArchivePasswordStore,
    cancellation: tokio_util::sync::CancellationToken,
) -> Result<AgentSourcePreparation, String> {
    let root_id = resolve_scope_root(&registry, selected_id)?;
    let loader = LogSourceLoader::new(loader_config.clone())
        .with_archive_passwords(archive_passwords.clone());
    let mut pending_ids = vec![root_id];
    let mut visited_ids = BTreeSet::new();
    let mut warnings = BTreeSet::new();

    while let Some(source_id) = pending_ids.pop() {
        if cancellation.is_cancelled() {
            return Err("来源树完整扫描已取消".to_string());
        }
        if !visited_ids.insert(source_id) {
            continue;
        }
        let Some(node) = registry.node(source_id).cloned() else {
            continue;
        };

        // 完整扫描只关心是否已经加载，不依赖节点的 expanded UI 状态；克隆树中的旧 loading 标记也由本任务接管。
        if node.kind.can_expand() && !node.metadata.children_loaded {
            registry.set_loading(source_id, true);
            let report = loader.load_children(&node);
            apply_scan_child_report(&mut registry, source_id, report, &mut warnings);
        }

        // 子级可能刚由上一步挂回注册表，必须重新读取索引并逆序入栈以保持原始树序遍历。
        for child_id in registry.child_ids(source_id).iter().rev().copied() {
            pending_ids.push(child_id);
        }
    }

    if cancellation.is_cancelled() {
        return Err("来源树完整扫描已取消".to_string());
    }

    let warnings = warnings.into_iter().collect::<Vec<_>>();
    let scope = SourceScopeSnapshot::from_registry(
        &registry,
        selected_id,
        &config,
        default_encoding,
        loader_config,
        archive_passwords,
    )
    .map_err(|error| {
        if warnings.is_empty() {
            error
        } else {
            format!("{error}；扫描警告：{}", warnings.join("；"))
        }
    })?;
    let match_summaries = build_match_summaries(&config.log_profiles, &scope);

    Ok(AgentSourcePreparation {
        registry,
        scope,
        warnings,
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
                    if matcher.is_match(&source.file_name, &source.relative_path) {
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

/// 解析当前分析根；多根来源继续要求明确选择，避免无提示扩大日志授权范围。
fn resolve_scope_root(
    registry: &SourceRegistry,
    selected_id: Option<SourceId>,
) -> Result<SourceId, String> {
    match selected_id {
        Some(source_id) => registry
            .root_id_for(source_id)
            .ok_or_else(|| "当前选中来源不存在，无法确定 AI 分析范围".to_string()),
        None if registry.root_ids().len() == 1 => Ok(registry.root_ids()[0]),
        None if registry.root_ids().is_empty() => Err("请先加载日志来源".to_string()),
        None => Err("存在多个来源根，请先在来源树中选择要分析的范围".to_string()),
    }
}

/// 把一次子级加载报告挂回扫描副本，并保留局部错误供启动轨迹提示。
fn apply_scan_child_report(
    registry: &mut SourceRegistry,
    parent_id: SourceId,
    report: LoadReport,
    warnings: &mut BTreeSet<String>,
) {
    let LoadReport {
        registry: child_registry,
        errors,
        password_request,
        ..
    } = report;
    warnings.extend(errors.iter().cloned());
    if let Some(password_request) = password_request {
        warnings.insert(format!("归档需要密码：{password_request}"));
    }

    if child_registry.is_empty() && !errors.is_empty() {
        registry.mark_children_load_failed(parent_id, errors.join("；"));
        return;
    }

    let should_keep_expanded = registry
        .node(parent_id)
        .map(|node| node.expanded)
        .unwrap_or(false);
    registry.append_children_registry(parent_id, child_registry, should_keep_expanded);
    if let Some(parent) = registry.node_mut(parent_id)
        && !errors.is_empty()
    {
        parent.metadata.message = Some(errors.join("；"));
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;
    use uuid::Uuid;

    use super::*;
    use crate::config::{LogNameMatcher, LogNameMatcherMode, LogNameMatcherTarget, LogTypeProfile};
    use crate::loader::{SourceKind, SourceLocation, SourceMetadata, SourceTreeNode};

    /// 验证未展开且尚未加载的目录会在后台扫描中补齐，并按文件名匹配日志类型说明。
    #[test]
    fn source_scan_loads_collapsed_directory_and_matches_profile() {
        let directory = tempdir().expect("应创建临时日志目录");
        fs::write(directory.path().join("application.log"), "startup failed")
            .expect("应写入测试日志");

        let mut registry = SourceRegistry::new();
        let root_id = registry.allocate_id();
        registry.insert_node(SourceTreeNode {
            id: root_id,
            parent_id: None,
            depth: 0,
            label: "logs".to_string(),
            kind: SourceKind::Directory,
            location: SourceLocation::LocalPath(directory.path().to_path_buf()),
            metadata: SourceMetadata::default(),
            selected: false,
            expanded: false,
        });
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

        let preparation = prepare_agent_source_scope(
            registry,
            None,
            config,
            "UTF-8".to_string(),
            LoaderConfig::default(),
            ArchivePasswordStore::default(),
            tokio_util::sync::CancellationToken::new(),
        )
        .expect("折叠目录应被完整扫描");

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
        assert!(!preparation.registry.node(root_id).unwrap().expanded);
        assert!(
            preparation
                .registry
                .node(root_id)
                .unwrap()
                .metadata
                .children_loaded
        );
    }

    /// 验证每条规则独立计数，未命中规则保留零值，且同一文件可命中多条规则。
    #[test]
    fn source_scan_reports_each_matcher_hit_count() {
        let directory = tempdir().expect("应创建临时日志目录");
        fs::write(
            directory.path().join("memory_20260715.log"),
            "memory pressure",
        )
        .expect("应写入内存日志");
        fs::write(
            directory.path().join("application.log"),
            "application started",
        )
        .expect("应写入应用日志");

        let mut registry = SourceRegistry::new();
        let root_id = registry.allocate_id();
        registry.insert_node(SourceTreeNode {
            id: root_id,
            parent_id: None,
            depth: 0,
            label: "logs".to_string(),
            kind: SourceKind::Directory,
            location: SourceLocation::LocalPath(directory.path().to_path_buf()),
            metadata: SourceMetadata::default(),
            selected: false,
            expanded: false,
        });
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

        let preparation = prepare_agent_source_scope(
            registry,
            None,
            config,
            "UTF-8".to_string(),
            LoaderConfig::default(),
            ArchivePasswordStore::default(),
            tokio_util::sync::CancellationToken::new(),
        )
        .expect("来源扫描应成功");
        let summary = &preparation.match_summaries[0];

        assert_eq!(summary.matched_file_count, 2);
        assert_eq!(summary.selected_file_count, 2);
        assert_eq!(summary.rules[0].matched_file_count, 1);
        assert_eq!(summary.rules[1].matched_file_count, 2);
        assert_eq!(summary.rules[2].matched_file_count, 0);
    }

    /// 验证启动对话框关闭后，已取消的来源扫描不会继续构建或回填会话范围。
    #[test]
    fn source_scan_stops_when_cancelled_before_start() {
        let directory = tempdir().expect("应创建临时日志目录");
        fs::write(directory.path().join("application.log"), "started").expect("应写入测试日志");
        let mut registry = SourceRegistry::new();
        let root_id = registry.allocate_id();
        registry.insert_node(SourceTreeNode {
            id: root_id,
            parent_id: None,
            depth: 0,
            label: "logs".to_string(),
            kind: SourceKind::Directory,
            location: SourceLocation::LocalPath(directory.path().to_path_buf()),
            metadata: SourceMetadata::default(),
            selected: false,
            expanded: false,
        });
        registry.rebuild_all_indices();
        let cancellation = tokio_util::sync::CancellationToken::new();
        cancellation.cancel();

        let error = prepare_agent_source_scope(
            registry,
            None,
            AiConfig::default(),
            "UTF-8".to_string(),
            LoaderConfig::default(),
            ArchivePasswordStore::default(),
            cancellation,
        )
        .expect_err("取消后的扫描必须停止");

        assert!(error.contains("已取消"));
    }
}
