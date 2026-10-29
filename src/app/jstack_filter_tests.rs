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
            thread_name: "worker".to_string(),
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
