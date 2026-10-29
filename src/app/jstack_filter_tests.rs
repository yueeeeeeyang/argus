//! 文件职责：验证 Jstack 样本过滤与矩阵缓存、详情和交互状态的一致性。
//! 创建日期：2026-10-29
//! 修改日期：2026-10-29
//! 作者：Argus 开发团队

use super::*;
use crate::analysis::jstack::{JstackVisibleCell, build_analysis_result, parse_jstack_snapshot};
use crate::config::JstackThreadFilterRuleKind;
use crate::config::paths::isolated_test_dir;
use crate::ui::jstack_analysis_view::JstackCellPreviewData;

/// 经真实解析器构造线程块，保证状态和出现序号来自生产聚合路径。
fn thread_sample(name: &str, id: usize, state: &str, frame: &str) -> String {
    format!("\"{name}\" #{id}\n   java.lang.Thread.State: {state}\n        at {frame}\n")
}

/// 保留输入快照顺序；空字符串用于模拟线程缺席的快照。
fn analysis_result(texts: &[&str]) -> JstackAnalysisResult {
    let snapshots = texts
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let label = format!("{:03}.log", index + 1);
            parse_jstack_snapshot(
                SourceId(index + 1),
                label.clone(),
                format!("/tmp/{label}"),
                text,
            )
        })
        .collect();
    build_analysis_result(snapshots, Vec::new(), texts.len())
}

/// 构造不涉及后台任务和真实来源的分析状态，允许独立验证缓存重建。
fn analysis_state(result: JstackAnalysisResult) -> JstackAnalysisState {
    JstackAnalysisState {
        generation: 1,
        active_states: BTreeSet::from([JstackThreadState::Runnable]),
        is_thread_filter_enabled: true,
        thread_name_selection: None,
        thread_name_selection_drag: None,
        selected_cell_key: None,
        visible_row_indices: Vec::new(),
        visible_cells: HashMap::new(),
        filtered_row_count: 0,
        row_scroll: UniformListScrollHandle::new(),
        task_state: JstackAnalysisTaskState::Ready(result),
        cancel_token: None,
    }
}

/// 规则由测试显式提供，避免默认配置变化影响过滤行为断言。
fn stack_rule(pattern: &str) -> JstackThreadFilterRule {
    JstackThreadFilterRule {
        enabled: true,
        kind: JstackThreadFilterRuleKind::StackSegment,
        pattern: pattern.to_string(),
    }
}

/// 从原始结果定位线程行，避免测试依赖聚合器初始排序。
fn row_index(state: &JstackAnalysisState, name: &str) -> usize {
    let JstackAnalysisTaskState::Ready(result) = &state.task_state else {
        panic!("测试分析必须已完成");
    };
    result
        .rows
        .iter()
        .position(|row| row.thread_name == name)
        .expect("应存在测试线程")
}

/// 动作测试使用独立配置目录，避免写入用户配置。
fn app_with_analysis(result: JstackAnalysisResult, rules: Vec<JstackThreadFilterRule>) -> ArgusApp {
    let config_dir = isolated_test_dir("jstack-filter-actions");
    let mut app =
        ArgusApp::new_with_config_manager(ConfigManager::new(config_dir.join("settings.toml")));
    app.config.log_display.jstack_thread_filter_rules = rules;
    app.jstack_analyses.insert(1, analysis_state(result));
    app.rebuild_all_jstack_visible_row_caches();
    app
}

/// 模拟规则修改前已经展示的预览，验证动作必须主动清除旧堆栈。
fn seed_hover(app: &mut ArgusApp) {
    app.show_jstack_cell_hover_preview(JstackCellHoverPreview {
        key: "1:0:0".to_string(),
        analysis_id: 1,
        position: point(px(0.0), px(0.0)),
        data: JstackCellPreviewData {
            snapshot_label: "001.log".to_string(),
            thread_label: "worker".to_string(),
            count: 1,
            state_label: "RUNNABLE".to_string(),
            stack_lines: Some(Arc::from(vec!["app.Idle.wait(Idle.java:1)".to_string()])),
        },
    });
}

/// 一个快照等待不能隐藏另一个快照的业务样本，且输入顺序不改变规则语义。
#[test]
fn sample_filter_preserves_busy_snapshot_in_both_orders() {
    let idle = thread_sample("worker", 7, "RUNNABLE", "app.Idle.wait(Idle.java:1)");
    let busy = thread_sample("worker", 7, "RUNNABLE", "app.Business.run(Business.java:2)");
    let filter = JstackThreadFilter::from_rules(&[stack_rule("app.Idle.wait")]);

    for texts in [
        [idle.as_str(), busy.as_str()],
        [busy.as_str(), idle.as_str()],
    ] {
        let original = analysis_result(&texts);
        let mut state = analysis_state(original.clone());
        state.rebuild_visible_row_cache(&filter);
        let index = row_index(&state, "worker");

        assert_eq!(state.visible_row_indices, vec![index]);
        assert_eq!(state.filtered_row_count, 0);
        for (snapshot_index, text) in texts.iter().enumerate() {
            let expected = if *text == busy {
                JstackVisibleCell {
                    count: 1,
                    state: Some(JstackThreadState::Runnable),
                    representative_occurrence_index: Some(1),
                }
            } else {
                JstackVisibleCell::default()
            };
            assert_eq!(state.visible_cells[&index][snapshot_index], expected);
        }
        let JstackAnalysisTaskState::Ready(result) = &state.task_state else {
            panic!("缓存重建不能改变任务状态");
        };
        assert_eq!(result, &original, "缓存筛选不能修改原始结果");
    }
}

/// 空快照保留列位置，但不成为线程被整行过滤或保留的理由。
#[test]
fn sample_filter_handles_missing_snapshots_without_renumbering() {
    let busy = thread_sample("worker", 7, "RUNNABLE", "app.Business.run(Business.java:2)");
    let idle = thread_sample("worker", 7, "RUNNABLE", "app.Idle.wait(Idle.java:1)");
    let filter = JstackThreadFilter::from_rules(&[stack_rule("app.Idle.wait")]);
    let mut state = analysis_state(analysis_result(&["", &idle, "", &busy]));
    state.rebuild_visible_row_cache(&filter);
    let index = row_index(&state, "worker");

    assert_eq!(state.visible_row_indices, vec![index]);
    assert_eq!(state.filtered_row_count, 0);
    assert_eq!(
        state.visible_cells[&index]
            .iter()
            .map(|cell| cell.count)
            .collect::<Vec<_>>(),
        vec![0, 0, 0, 1]
    );
    let JstackAnalysisTaskState::Ready(result) = &state.task_state else {
        panic!("测试分析必须已完成");
    };
    let occurrences = jstack_detail_occurrences_for_visible_cells(
        &result.rows[index],
        &state.active_states,
        3,
        1,
        Some(&filter),
    );
    assert_eq!(occurrences.len(), 1);
    assert_eq!(occurrences[0].snapshot_index, 3);

    let mut all_idle = analysis_state(analysis_result(&["", &idle, ""]));
    all_idle.rebuild_visible_row_cache(&filter);
    assert!(all_idle.visible_row_indices.is_empty());
    assert!(all_idle.visible_cells.is_empty());
    assert_eq!(all_idle.filtered_row_count, 1);
}

/// 同一格子的等待样本被排除后，计数、主状态和代表序号必须一起重算。
#[test]
fn sample_filter_recomputes_cell_count_state_and_representative() {
    let idle = thread_sample("worker", 7, "WAITING", "app.Idle.wait(Idle.java:1)");
    let busy = thread_sample("worker", 7, "RUNNABLE", "app.Business.run(Business.java:2)");
    let text = format!("{idle}{idle}{busy}");
    let mut state = analysis_state(analysis_result(&[&text]));
    state.active_states.insert(JstackThreadState::Waiting);
    state.rebuild_visible_row_cache(&JstackThreadFilter::from_rules(&[stack_rule(
        "app.Idle.wait",
    )]));
    let index = row_index(&state, "worker");

    assert_eq!(
        state.visible_cells[&index][0],
        JstackVisibleCell {
            count: 1,
            state: Some(JstackThreadState::Runnable),
            representative_occurrence_index: Some(3),
        }
    );
    let JstackAnalysisTaskState::Ready(result) = &state.task_state else {
        panic!("测试分析必须已完成");
    };
    assert_eq!(result.rows[index].cells[0].count, 3);
    assert_eq!(
        result.rows[index].cells[0].state,
        Some(JstackThreadState::Waiting)
    );
}

/// 状态选择作用于样本，原始主状态为 WAITING 也不能吞掉同格的 RUNNABLE。
#[test]
fn sample_state_filter_preserves_runnable_in_waiting_dominant_cell() {
    let waiting = thread_sample("worker", 7, "WAITING", "app.Queue.take(Queue.java:1)");
    let runnable = thread_sample("worker", 7, "RUNNABLE", "app.Business.run(Business.java:2)");
    let mut state = analysis_state(analysis_result(&[&format!("{waiting}{waiting}{runnable}")]));
    let filter = JstackThreadFilter::from_rules(&[]);
    state.rebuild_visible_row_cache(&filter);
    let index = row_index(&state, "worker");

    assert_eq!(state.visible_row_indices, vec![index]);
    assert_eq!(state.visible_cells[&index][0].count, 1);
    assert_eq!(
        state.visible_cells[&index][0].state,
        Some(JstackThreadState::Runnable)
    );
    assert_eq!(
        state.visible_cells[&index][0].representative_occurrence_index,
        Some(3)
    );

    state.active_states = BTreeSet::from([JstackThreadState::Waiting]);
    state.rebuild_visible_row_cache(&filter);
    assert_eq!(state.visible_cells[&index][0].count, 2);
    assert_eq!(
        state.visible_cells[&index][0].state,
        Some(JstackThreadState::Waiting)
    );
    assert_eq!(
        state.visible_cells[&index][0].representative_occurrence_index,
        Some(1)
    );
}

/// 多条堆栈规则可联合排除整行；隐藏计数独立于当前状态选择。
#[test]
fn sample_filter_counts_only_rows_fully_covered_by_enabled_rules() {
    let accept = thread_sample("worker", 7, "RUNNABLE", "app.Idle.accept(Idle.java:1)");
    let park = thread_sample("worker", 7, "WAITING", "app.Idle.park(Idle.java:2)");
    let mut state = analysis_state(analysis_result(&[&accept, "", &park]));
    let mut rules = vec![stack_rule("app.Idle.accept"), stack_rule("app.Idle.park")];
    let filter = JstackThreadFilter::from_rules(&rules);
    state.rebuild_visible_row_cache(&filter);
    assert!(state.visible_row_indices.is_empty());
    assert!(state.visible_cells.is_empty());
    assert_eq!(state.filtered_row_count, 1);

    state.active_states.clear();
    state.rebuild_visible_row_cache(&filter);
    assert_eq!(state.filtered_row_count, 1);
    assert!(state.visible_row_indices.is_empty());

    rules[1].enabled = false;
    state.rebuild_visible_row_cache(&JstackThreadFilter::from_rules(&rules));
    assert_eq!(state.filtered_row_count, 0, "仍有样本未被配置规则排除");
    assert!(
        state.visible_row_indices.is_empty(),
        "状态全关闭时不展示任何样本"
    );
    state.active_states.insert(JstackThreadState::Waiting);
    state.rebuild_visible_row_cache(&JstackThreadFilter::from_rules(&rules));
    let index = row_index(&state, "worker");
    assert_eq!(state.visible_row_indices, vec![index]);
    assert_eq!(state.visible_cells[&index][2].count, 1);
}

/// 线程名规则仍隐藏整个线程身份，详情也必须遵循相同规则。
#[test]
fn thread_name_filter_excludes_busy_cells_and_detail() {
    let busy = thread_sample(
        "http-worker",
        7,
        "RUNNABLE",
        "app.Business.run(Business.java:2)",
    );
    let mut state = analysis_state(analysis_result(&[&busy, &busy]));
    let filter = JstackThreadFilter::from_rules(&[JstackThreadFilterRule {
        enabled: true,
        kind: JstackThreadFilterRuleKind::ThreadName,
        pattern: "http-*".to_string(),
    }]);
    state.rebuild_visible_row_cache(&filter);
    assert!(state.visible_row_indices.is_empty());
    assert!(state.visible_cells.is_empty());
    assert_eq!(state.filtered_row_count, 1);
    let JstackAnalysisTaskState::Ready(result) = &state.task_state else {
        panic!("测试分析必须已完成");
    };
    assert!(
        jstack_detail_occurrences_for_visible_cells(
            &result.rows[0],
            &state.active_states,
            0,
            1,
            Some(&filter),
        )
        .is_empty()
    );
}

/// 详情每个可见快照只取一个代表，并拒绝旧点击序号指向的已过滤样本。
#[test]
fn detail_never_reintroduces_filtered_occurrences() {
    let idle = thread_sample("worker", 7, "RUNNABLE", "app.Idle.wait(Idle.java:1)");
    let first_busy = thread_sample(
        "worker",
        7,
        "RUNNABLE",
        "app.Business.first(Business.java:2)",
    );
    let second_busy = thread_sample(
        "worker",
        7,
        "RUNNABLE",
        "app.Business.second(Business.java:3)",
    );
    let mixed = format!("{idle}{first_busy}{second_busy}");
    let result = analysis_result(&[&idle, &mixed, &first_busy]);
    let row = &result.rows[0];
    let active_states = BTreeSet::from([JstackThreadState::Runnable]);
    let filter = JstackThreadFilter::from_rules(&[stack_rule("app.Idle.wait")]);

    let occurrences =
        jstack_detail_occurrences_for_visible_cells(row, &active_states, 1, 1, Some(&filter));
    assert_eq!(
        occurrences
            .iter()
            .map(|sample| (sample.snapshot_index, sample.occurrence_index))
            .collect::<Vec<_>>(),
        vec![(1, 2), (2, 1)]
    );
    assert!(
        occurrences
            .iter()
            .all(|sample| !sample.normalized_stack_text.contains("app.idle.wait"))
    );

    let selected =
        jstack_detail_occurrences_for_visible_cells(row, &active_states, 1, 3, Some(&filter));
    assert_eq!(selected[0].occurrence_index, 3, "合法点击必须保留原始序号");
    let restored = jstack_detail_occurrences_for_visible_cells(row, &active_states, 1, 1, None);
    assert_eq!(restored.len(), 3);
    assert_eq!(restored[1].occurrence_index, 1);
    assert!(restored[1].normalized_stack_text.contains("app.idle.wait"));
    assert!(
        jstack_detail_occurrences_for_visible_cells(row, &BTreeSet::new(), 1, 3, Some(&filter))
            .is_empty()
    );
}

/// 状态过滤后的详情代表必须与矩阵代表相同，而不是原始主状态或首条记录。
#[test]
fn detail_uses_visible_state_representative_in_mixed_cell() {
    let waiting = thread_sample("worker", 7, "WAITING", "app.Queue.take(Queue.java:1)");
    let busy = thread_sample("worker", 7, "RUNNABLE", "app.Business.run(Business.java:2)");
    let result = analysis_result(&[&format!("{waiting}{waiting}{busy}")]);
    let row = &result.rows[0];
    let active_states = BTreeSet::from([JstackThreadState::Runnable]);
    let occurrences = jstack_detail_occurrences_for_visible_cells(row, &active_states, 0, 1, None);
    assert_eq!(occurrences.len(), 1);
    assert_eq!(occurrences[0].state, JstackThreadState::Runnable);
    assert_eq!(occurrences[0].occurrence_index, 3);
    assert_eq!(
        Some(occurrences[0].occurrence_index),
        row.cells[0]
            .visible_summary(&active_states, None)
            .representative_occurrence_index
    );
}

/// 排序按剩余样本计数，同次数时以名称和线程 ID 稳定排序；重建不能保留旧摘要。
#[test]
fn sample_filter_rebuilds_sorted_cache_without_stale_cells() {
    let idle = thread_sample(
        "original-leader",
        1,
        "RUNNABLE",
        "app.Idle.wait(Idle.java:1)",
    );
    let leader = thread_sample(
        "original-leader",
        1,
        "RUNNABLE",
        "app.Business.run(Business.java:2)",
    );
    let busiest = thread_sample(
        "visible-leader",
        2,
        "RUNNABLE",
        "app.Business.run(Business.java:2)",
    );
    let alpha = thread_sample("alpha", 3, "RUNNABLE", "app.Business.run(Business.java:2)");
    let duplicate_two = thread_sample(
        "same-name",
        2,
        "RUNNABLE",
        "app.Business.run(Business.java:2)",
    );
    let duplicate_one = thread_sample(
        "same-name",
        1,
        "RUNNABLE",
        "app.Business.run(Business.java:2)",
    );
    let text = format!(
        "{idle}{idle}{idle}{leader}{busiest}{busiest}{alpha}{duplicate_two}{duplicate_one}"
    );
    let mut state = analysis_state(analysis_result(&[&text]));
    let filter = JstackThreadFilter::from_rules(&[stack_rule("app.Idle.wait")]);
    state.rebuild_visible_row_cache(&filter);
    let JstackAnalysisTaskState::Ready(result) = &state.task_state else {
        panic!("测试分析必须已完成");
    };
    let identities = state
        .visible_row_indices
        .iter()
        .map(|index| {
            let row = &result.rows[*index];
            (row.thread_name.as_str(), row.thread_id.as_str())
        })
        .collect::<Vec<_>>();
    assert_eq!(
        identities,
        vec![
            ("visible-leader", "#2"),
            ("alpha", "#3"),
            ("original-leader", "#1"),
            ("same-name", "#1"),
            ("same-name", "#2")
        ]
    );
    let expected_indices = state.visible_row_indices.clone();
    let expected_cells = state.visible_cells.clone();
    state.selected_cell_key = Some(jstack_cell_selection_key(expected_indices[0], 0));
    state.thread_name_selection = Some(JstackThreadNameSelection {
        thread_identity: "visible-leader · #2".to_string(),
        thread_name: "visible-leader".to_string(),
        anchor: 0,
        focus: 7,
    });
    state.thread_name_selection_drag = Some(JstackThreadNameSelectionDrag {
        thread_identity: "visible-leader · #2".to_string(),
        thread_name: "visible-leader".to_string(),
        anchor_range: 0..7,
        granularity: TextSelectionGranularity::Character,
    });
    state.rebuild_visible_row_cache(&filter);
    assert_eq!(state.visible_row_indices, expected_indices);
    assert_eq!(state.visible_cells, expected_cells);
    assert!(state.selected_cell_key.is_none());
    assert!(state.thread_name_selection.is_none());
    assert!(state.thread_name_selection_drag.is_none());

    state.active_states.clear();
    state.rebuild_visible_row_cache(&filter);
    assert!(state.visible_row_indices.is_empty());
    assert!(state.visible_cells.is_empty());
    assert_eq!(state.filtered_row_count, 0);
    state.active_states.insert(JstackThreadState::Runnable);
    state.rebuild_visible_row_cache(&filter);
    assert!(!state.visible_cells.is_empty());
    state.task_state = JstackAnalysisTaskState::Cancelled {
        message: "已取消测试分析".to_string(),
    };
    state.rebuild_visible_row_cache(&filter);
    assert!(state.visible_row_indices.is_empty());
    assert!(state.visible_cells.is_empty());
    assert_eq!(state.filtered_row_count, 0);
}

/// 页面关闭配置过滤后恢复原样本，切换过程中清除旧悬浮和选中格子。
#[test]
fn filter_toggle_restores_samples_and_clears_hover() {
    let idle = thread_sample("worker", 7, "RUNNABLE", "app.Idle.wait(Idle.java:1)");
    let busy = thread_sample("worker", 7, "RUNNABLE", "app.Business.run(Business.java:2)");
    let mut app = app_with_analysis(
        analysis_result(&[&format!("{idle}{busy}")]),
        vec![stack_rule("app.Idle.wait")],
    );
    let index = row_index(&app.jstack_analyses[&1], "worker");
    assert_eq!(app.jstack_analyses[&1].visible_cells[&index][0].count, 1);
    app.jstack_analyses
        .get_mut(&1)
        .expect("应存在分析状态")
        .selected_cell_key = Some(jstack_cell_selection_key(index, 0));
    seed_hover(&mut app);

    app.toggle_jstack_thread_filter(1);
    assert!(app.jstack_cell_hover_preview.is_none());
    let state = &app.jstack_analyses[&1];
    assert!(!state.is_thread_filter_enabled);
    assert!(state.selected_cell_key.is_none());
    assert_eq!(state.visible_cells[&index][0].count, 2);
    assert_eq!(
        state.visible_cells[&index][0].representative_occurrence_index,
        Some(1)
    );

    seed_hover(&mut app);
    app.toggle_jstack_thread_filter(1);
    assert!(app.jstack_cell_hover_preview.is_none());
    assert_eq!(app.jstack_analyses[&1].visible_cells[&index][0].count, 1);
    assert_eq!(
        app.jstack_analyses[&1].visible_cells[&index][0].representative_occurrence_index,
        Some(2)
    );
}

/// 状态选择和全局规则更新都使原预览失效，无需等待鼠标再次移动。
#[test]
fn state_and_rule_changes_clear_hover_and_rebuild_cache() {
    let busy = thread_sample("worker", 7, "RUNNABLE", "app.Business.run(Business.java:2)");
    let mut app = app_with_analysis(analysis_result(&[&busy]), Vec::new());
    seed_hover(&mut app);
    app.toggle_jstack_state_filter(1, JstackThreadState::Runnable);
    assert!(app.jstack_cell_hover_preview.is_none());
    assert!(app.jstack_analyses[&1].visible_cells.is_empty());
    app.toggle_jstack_state_filter(1, JstackThreadState::Runnable);
    assert_eq!(app.jstack_analyses[&1].visible_cells.len(), 1);

    seed_hover(&mut app);
    app.config.log_display.jstack_thread_filter_rules = vec![stack_rule("app.Business.run")];
    app.refresh_jstack_thread_filter();
    assert!(app.jstack_cell_hover_preview.is_none());
    assert!(app.jstack_analyses[&1].visible_row_indices.is_empty());
    assert!(app.jstack_analyses[&1].visible_cells.is_empty());
    assert_eq!(app.jstack_analyses[&1].filtered_row_count, 1);
    assert_eq!(app.jstack_filter_rule_hit_counts, vec![1]);

    app.config.log_display.jstack_thread_filter_rules.clear();
    app.refresh_jstack_thread_filter();
    assert_eq!(app.jstack_analyses[&1].visible_cells.len(), 1);
    assert_eq!(app.jstack_analyses[&1].filtered_row_count, 0);
    assert!(app.jstack_filter_rule_hit_counts.is_empty());
}

/// 使用应用线程转储格式，覆盖同名线程、重复采样、单线程缺席和整个快照缺席。
fn quoted_id_analysis_result() -> JstackAnalysisResult {
    let sample = |id: usize, frame: &str| {
        format!(
            "\"sync-worker\" id=\"{id}\" CPU Time=12.5ms User Time=10.0ms\n   java.lang.Thread.State: RUNNABLE\n        at {frame}\n"
        )
    };
    let first = format!(
        "{}{}{}",
        sample(128264, "app.IdentityA.first(IdentityA.java:1)"),
        sample(128263, "app.IdentityB.first(IdentityB.java:1)"),
        sample(128264, "app.IdentityA.second(IdentityA.java:2)"),
    );
    let second = sample(128263, "app.IdentityB.middle(IdentityB.java:2)");
    let last = format!(
        "{}{}",
        sample(128263, "app.IdentityB.last(IdentityB.java:3)"),
        sample(128264, "app.IdentityA.last(IdentityA.java:3)"),
    );
    analysis_result(&[&first, &second, "", &last])
}

/// 同名用例必须以名称和规范化 ID 一起定位，不能误取另一线程而掩盖身份合并缺陷。
fn row_index_for_identity(state: &JstackAnalysisState, name: &str, id: &str) -> usize {
    let JstackAnalysisTaskState::Ready(result) = &state.task_state else {
        panic!("测试分析必须已完成");
    };
    result
        .rows
        .iter()
        .position(|row| row.thread_name == name && row.thread_id == id)
        .expect("应存在指定名称和 ID 的线程")
}

/// 同名不同 ID 保持独立行和独立详情，缺席快照不缩短列位置或借用另一线程样本。
#[test]
fn quoted_thread_ids_keep_distinct_rows_and_details_across_missing_snapshots() {
    let mut state = analysis_state(quoted_id_analysis_result());
    state.rebuild_visible_row_cache(&JstackThreadFilter::from_rules(&[]));
    let first_index = row_index_for_identity(&state, "sync-worker", "#128264");
    let second_index = row_index_for_identity(&state, "sync-worker", "#128263");
    assert_ne!(first_index, second_index);
    assert_eq!(state.visible_row_indices.len(), 2);
    assert!(state.visible_row_indices.contains(&first_index));
    assert!(state.visible_row_indices.contains(&second_index));
    assert_eq!(state.filtered_row_count, 0);
    let JstackAnalysisTaskState::Ready(result) = &state.task_state else {
        panic!("测试分析必须已完成");
    };
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.snapshots.len(), 4);

    for (index, expected_counts) in [(first_index, [2, 0, 0, 1]), (second_index, [1, 1, 0, 1])] {
        let row = &result.rows[index];
        assert_eq!(row.total_count, 3);
        assert_eq!(row.cells.len(), 4);
        assert_eq!(
            row.cells
                .iter()
                .map(|cell| cell.snapshot_index)
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3],
        );
        assert_eq!(
            row.cells.iter().map(|cell| cell.count).collect::<Vec<_>>(),
            expected_counts,
        );
        assert_eq!(
            state.visible_cells[&index]
                .iter()
                .map(|cell| cell.count)
                .collect::<Vec<_>>(),
            expected_counts,
        );
        for (snapshot_index, count) in expected_counts.into_iter().enumerate() {
            let cell = &row.cells[snapshot_index];
            assert_eq!(
                cell.stack_occurrences
                    .iter()
                    .map(|occurrence| occurrence.occurrence_index)
                    .collect::<Vec<_>>(),
                (1..=count).collect::<Vec<_>>(),
                "每个线程身份在每个快照内独立编号",
            );
            if count == 0 {
                assert_eq!(cell.state, None);
                assert_eq!(
                    state.visible_cells[&index][snapshot_index],
                    JstackVisibleCell::default()
                );
            }
        }
    }

    for (index, selected_snapshot, selected_occurrence, expected_positions, expected_frames) in [
        (
            first_index,
            0,
            2,
            vec![(0, 2), (3, 1)],
            vec![
                "app.IdentityA.second(IdentityA.java:2)",
                "app.IdentityA.last(IdentityA.java:3)",
            ],
        ),
        (
            second_index,
            1,
            1,
            vec![(0, 1), (1, 1), (3, 1)],
            vec![
                "app.IdentityB.first(IdentityB.java:1)",
                "app.IdentityB.middle(IdentityB.java:2)",
                "app.IdentityB.last(IdentityB.java:3)",
            ],
        ),
    ] {
        let row = &result.rows[index];
        let occurrences = jstack_detail_occurrences_for_visible_cells(
            row,
            &state.active_states,
            selected_snapshot,
            selected_occurrence,
            None,
        );
        assert_eq!(
            occurrences
                .iter()
                .map(|occurrence| (occurrence.snapshot_index, occurrence.occurrence_index))
                .collect::<Vec<_>>(),
            expected_positions,
        );
        assert_eq!(occurrences.len(), expected_frames.len());
        for (occurrence, frame) in occurrences.iter().zip(expected_frames) {
            assert_eq!(occurrence.thread_name, row.thread_name);
            assert_eq!(occurrence.thread_id, row.thread_id);
            assert_eq!(
                occurrence.snapshot_label,
                result.snapshots[occurrence.snapshot_index].label
            );
            assert_eq!(
                occurrence.snapshot_path,
                result.snapshots[occurrence.snapshot_index].path
            );
            assert_eq!(
                occurrence
                    .stack_lines
                    .iter()
                    .filter_map(|line| line.trim().strip_prefix("at "))
                    .collect::<Vec<_>>(),
                vec![frame],
                "详情不能混入同名另一 ID 的堆栈",
            );
        }
    }
}

/// 名称规则仍作用于原始名称，并同时清理两个身份的可见缓存、选区和旧悬浮。
#[test]
fn quoted_thread_id_name_filter_hides_both_rows_and_clears_interaction() {
    let mut app = app_with_analysis(quoted_id_analysis_result(), Vec::new());
    let first_index = row_index_for_identity(&app.jstack_analyses[&1], "sync-worker", "#128264");
    let second_index = row_index_for_identity(&app.jstack_analyses[&1], "sync-worker", "#128263");
    let visible_indices = app.jstack_analyses[&1].visible_row_indices.clone();
    let visible_cells = app.jstack_analyses[&1].visible_cells.clone();
    app.begin_jstack_thread_name_selection(
        1,
        first_index.to_string(),
        "sync-worker".to_string(),
        0,
        TextSelectionGranularity::Character,
    );
    assert!(!app.update_jstack_thread_name_selection(1, &second_index.to_string(), 4));
    assert!(app.update_jstack_thread_name_selection(1, &first_index.to_string(), 4));
    app.jstack_analyses
        .get_mut(&1)
        .expect("应存在分析状态")
        .selected_cell_key = Some(jstack_cell_selection_key(first_index, 0));
    seed_hover(&mut app);

    let rule = JstackThreadFilterRule {
        enabled: true,
        kind: JstackThreadFilterRuleKind::ThreadName,
        pattern: "sync-worker".to_string(),
    };
    app.config.log_display.jstack_thread_filter_rules = vec![rule.clone()];
    app.refresh_jstack_thread_filter();
    assert!(app.jstack_cell_hover_preview.is_none());
    assert_eq!(app.jstack_filter_rule_hit_counts, vec![2]);
    let state = &app.jstack_analyses[&1];
    assert!(state.visible_row_indices.is_empty());
    assert!(state.visible_cells.is_empty());
    assert_eq!(state.filtered_row_count, 2);
    assert!(state.selected_cell_key.is_none());
    assert!(state.thread_name_selection.is_none());
    assert!(state.thread_name_selection_drag.is_none());
    let JstackAnalysisTaskState::Ready(result) = &state.task_state else {
        panic!("测试分析必须已完成");
    };
    let filter = JstackThreadFilter::from_rules(&[rule]);
    for row in &result.rows {
        assert!(
            jstack_detail_occurrences_for_visible_cells(
                row,
                &state.active_states,
                0,
                1,
                Some(&filter)
            )
            .is_empty()
        );
    }

    app.toggle_jstack_thread_filter(1);
    assert_eq!(app.jstack_analyses[&1].visible_row_indices, visible_indices);
    assert_eq!(app.jstack_analyses[&1].visible_cells, visible_cells);
    assert_eq!(app.jstack_analyses[&1].filtered_row_count, 0);
    app.toggle_jstack_thread_filter(1);
    assert!(app.jstack_analyses[&1].visible_row_indices.is_empty());

    // ID 只用于区分身份和展示，不能偷偷扩大已有“线程名”规则的匹配范围。
    app.config.log_display.jstack_thread_filter_rules[0].pattern = "#128264".to_string();
    app.refresh_jstack_thread_filter();
    assert_eq!(app.jstack_filter_rule_hit_counts, vec![0]);
    assert_eq!(app.jstack_analyses[&1].visible_row_indices, visible_indices);
    assert_eq!(app.jstack_analyses[&1].visible_cells, visible_cells);
}

/// 按堆栈排除一个身份时，同名另一身份的摘要与详情必须完整保留。
#[test]
fn quoted_thread_ids_keep_stack_filters_within_the_matching_identity() {
    let mut state = analysis_state(quoted_id_analysis_result());
    let first_index = row_index_for_identity(&state, "sync-worker", "#128264");
    let second_index = row_index_for_identity(&state, "sync-worker", "#128263");
    let filter = JstackThreadFilter::from_rules(&[stack_rule("app.IdentityA")]);
    state.rebuild_visible_row_cache(&filter);
    assert_eq!(state.filtered_row_count, 1);
    assert_eq!(state.visible_row_indices, vec![second_index]);
    assert!(!state.visible_cells.contains_key(&first_index));
    assert_eq!(
        state.visible_cells[&second_index]
            .iter()
            .map(|cell| cell.count)
            .collect::<Vec<_>>(),
        vec![1, 1, 0, 1]
    );
    let JstackAnalysisTaskState::Ready(result) = &state.task_state else {
        panic!("测试分析必须已完成");
    };
    assert!(
        jstack_detail_occurrences_for_visible_cells(
            &result.rows[first_index],
            &state.active_states,
            0,
            1,
            Some(&filter)
        )
        .is_empty()
    );
    let details = jstack_detail_occurrences_for_visible_cells(
        &result.rows[second_index],
        &state.active_states,
        0,
        1,
        Some(&filter),
    );
    assert_eq!(details.len(), 3);
    assert!(
        details
            .iter()
            .all(|occurrence| occurrence.thread_id == "#128263"
                && occurrence.normalized_stack_text.contains("app.identityb")
                && !occurrence.normalized_stack_text.contains("app.identitya"))
    );
}
