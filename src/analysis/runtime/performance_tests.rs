//! 文件职责：覆盖 Runtime 优化中的过滤语义、共享聚合、解析顺序和取消边界。

use super::*;

/// 同时包含同路径多请求、无 SQL 请求、元信息命中和不同 SQL 结构。
fn filter_fixture() -> RuntimeAnalysisResult {
    let definitions = [
        (
            "100&Alice&_api_alpha&1000&7&8.log",
            "/logs/metadata-marker.log",
            "95 1 2 3 4 SELECT * FROM users WHERE id = 1\n4 0 0 0 0 SELECT * FROM orders WHERE status = 'PAID'",
        ),
        (
            "200&Bob&_api_alpha&2000&9&10.log",
            "/logs/b.log",
            "10 1 2 3 4 SELECT * FROM users WHERE id = 2",
        ),
        (
            "300&Alice&_api_alpha&3000&11&12.log",
            "/logs/c.log",
            "20 1 2 3 4 SELECT * FROM orders WHERE status = 'NEW'",
        ),
        ("400&用户&_api_empty&4000&13&14.log", "/logs/empty.log", ""),
        (
            "500&Alice&_api_beta&5000&15&16.log",
            "/logs/e.log",
            "35 1 2 3 4 SELECT * FROM users WHERE id = 3",
        ),
    ];
    let requests = definitions
        .into_iter()
        .enumerate()
        .map(|(index, (label, path, sql))| {
            let mut request = parse_runtime_request_text(SourceId(index + 1), label, path, sql)
                .expect("应能构建过滤样本");
            request.index = index;
            request
        })
        .collect();
    build_runtime_analysis_result(requests, Vec::new(), definitions.len(), false)
}

/// 独立沿用逐层匹配规则作对照，覆盖汇总自身命中、用户名、时间、空 SQL 与跨字段关键字。
#[test]
fn single_pass_filter_preserves_layered_matching_semantics() {
    let result = filter_fixture();
    let keywords = [
        "",
        "metadata-marker",
        "alice 100 ms",
        "id = 2",
        "id = ?",
        "3 /api/alpha",
        "200.0 ms",
        "33.3%",
        "用户",
        "missing",
    ];
    let cross_filters = [
        ("", "", ""),
        ("alice", "", ""),
        ("bob，用户", "", ""),
        ("", "2000", "4000"),
        ("alice", "3000", "1000"),
        ("", "invalid", "invalid"),
    ];
    for keyword in keywords {
        for (username, start_time, end_time) in cross_filters {
            let filter = RuntimeAnalysisFilterSnapshot {
                keyword: keyword.into(),
                username: username.into(),
                start_time: start_time.into(),
                end_time: end_time.into(),
            };
            let criteria = parse_runtime_analysis_filter_criteria(&filter);
            let expected_summaries = result
                .summaries
                .iter()
                .filter_map(|summary| {
                    filtered_runtime_summary_from_indices(&result, summary, &criteria, true)
                })
                .collect::<Vec<_>>();
            let expected_sql = result
                .requests
                .iter()
                .filter_map(|request| {
                    if !runtime_request_matches_cross_filters(request, &criteria) {
                        return None;
                    }
                    let indices = request
                        .sql_records
                        .iter()
                        .enumerate()
                        .filter(|(_, sql)| runtime_sql_matches_keyword(request, sql, &criteria))
                        .map(|(index, _)| index)
                        .collect::<Vec<_>>();
                    (!indices.is_empty()).then_some((request.index, indices))
                })
                .collect::<HashMap<_, _>>();
            let actual = build_runtime_analysis_filter_rows(
                &result,
                filter.clone(),
                &AtomicBool::new(false),
            )
            .expect("未取消的过滤应完成");
            assert_eq!(*actual.summaries, expected_summaries, "{filter:?}");
            assert_eq!(*actual.sql_indices_by_request, expected_sql, "{filter:?}");
            assert_eq!(
                *actual.summary_indices_by_path,
                runtime_summary_indices_by_path(&expected_summaries)
            );
        }
    }
}

/// 汇总自身命中保留整组，SQL 仍独立过滤；原分组未变化时共享索引而非深复制。
#[test]
fn summary_keyword_keeps_group_and_shares_unchanged_indices() {
    let result = filter_fixture();
    let filter = RuntimeAnalysisFilterSnapshot {
        keyword: "3 /api/alpha".into(),
        ..Default::default()
    };
    let rows =
        build_runtime_analysis_filter_rows(&result, filter, &AtomicBool::new(false)).unwrap();
    assert_eq!(rows.summaries.len(), 1);
    assert_eq!(rows.summaries[0].request_indices.as_ref(), &[0, 1, 2]);
    assert!(rows.sql_indices_by_request.is_empty());
    let original = &result.summaries[result.summary_indices_by_path["/api/alpha"]];
    assert!(Arc::ptr_eq(
        &rows.summaries[0].request_indices,
        &original.request_indices
    ));
    let cloned = rows.clone();
    assert!(Arc::ptr_eq(&rows.summaries, &cloned.summaries));
    assert!(Arc::ptr_eq(
        &rows.sql_indices_by_request,
        &cloned.sql_indices_by_request
    ));
    assert!(Arc::ptr_eq(
        &rows.summary_indices_by_path,
        &cloned.summary_indices_by_path
    ));
}

/// 共享聚合精确使用可见 SQL，保留两种排序；过期过滤缓存不影响当前过滤结果。
#[test]
fn shared_sql_aggregation_matches_reference_and_rejects_stale_filter_cache() {
    let result = filter_fixture();
    let stale_filter = RuntimeAnalysisFilterSnapshot {
        keyword: "missing".into(),
        ..Default::default()
    };
    let stale_rows =
        build_runtime_analysis_filter_rows(&result, stale_filter, &AtomicBool::new(false)).unwrap();
    for filter in [
        RuntimeAnalysisFilterSnapshot::default(),
        RuntimeAnalysisFilterSnapshot {
            keyword: "users".into(),
            username: "alice".into(),
            ..Default::default()
        },
        RuntimeAnalysisFilterSnapshot {
            keyword: "metadata-marker".into(),
            ..Default::default()
        },
        RuntimeAnalysisFilterSnapshot {
            keyword: "33.3%".into(),
            ..Default::default()
        },
    ] {
        let rows =
            build_runtime_analysis_filter_rows(&result, filter.clone(), &AtomicBool::new(false))
                .unwrap();
        let criteria = parse_runtime_analysis_filter_criteria(&filter);
        let mut expected = std::collections::BTreeMap::<String, (u64, usize)>::new();
        for request in &result.requests {
            if !runtime_request_matches_cross_filters(request, &criteria) {
                continue;
            }
            for sql in &request.sql_records {
                if runtime_sql_matches_keyword(request, sql, &criteria) {
                    let totals = expected.entry(sql.normalized_sql.clone()).or_default();
                    totals.0 += sql.execute_ms;
                    totals.1 += 1;
                }
            }
        }
        let mut frequency = expected
            .iter()
            .map(|(sql, (total, count))| RuntimeSqlFrequencyAnalysisRow {
                normalized_sql: sql.clone(),
                total_execute_ms: *total,
                execute_count: *count,
            })
            .collect::<Vec<_>>();
        let mut slow = expected
            .into_iter()
            .map(|(sql, (total, count))| RuntimeSlowSqlSummaryRow {
                normalized_sql: sql,
                total_execute_ms: total,
                execute_count: count,
            })
            .collect::<Vec<_>>();
        sort_runtime_sql_frequency_rows(&mut frequency);
        sort_runtime_slow_sql_summary_rows(&mut slow);
        for supplied in [None, Some(&rows), Some(&stale_rows)] {
            let actual = build_runtime_sql_analysis_rows(
                &result,
                &filter,
                supplied,
                &AtomicBool::new(false),
            )
            .unwrap();
            assert_eq!(*actual.frequency, frequency, "{filter:?}");
            assert_eq!(*actual.slow, slow, "{filter:?}");
        }
    }
}

/// 取消不能表现为成功空结果；无请求的正常输入则必须完成为空结果。
#[test]
fn cancelled_and_empty_analysis_results_remain_distinct() {
    let result = filter_fixture();
    let filter = RuntimeAnalysisFilterSnapshot::default();
    let cancelled = AtomicBool::new(true);
    assert!(build_runtime_analysis_filter_rows(&result, filter.clone(), &cancelled).is_none());
    assert!(build_runtime_sql_analysis_rows(&result, &filter, None, &cancelled).is_none());
    let empty = build_runtime_analysis_result(Vec::new(), Vec::new(), 0, false);
    let rows = build_runtime_analysis_filter_rows(&empty, filter.clone(), &AtomicBool::new(false))
        .unwrap();
    assert!(rows.summaries.is_empty());
    assert!(rows.summary_indices_by_path.is_empty());
    let sql =
        build_runtime_sql_analysis_rows(&empty, &filter, Some(&rows), &AtomicBool::new(false))
            .unwrap();
    assert!(sql.frequency.is_empty());
    assert!(sql.slow.is_empty());
}

/// 在已读完部分 SQL 后取消，下一行和结束动作均拒绝发布已有部分结果。
#[test]
fn parser_checks_cancellation_between_lines_and_before_finishing() {
    let mut cache = RuntimeSqlCache::default();
    let cancel = AtomicBool::new(false);
    let mut parser = RuntimeSqlParser::new(&mut cache, &cancel);
    parser.push_line("1 0 0 0 0 select 1").unwrap();
    parser.push_line("2 0 0 0 0 select 2").unwrap();
    assert_eq!(parser.records.len(), 1);
    cancel.store(true, Ordering::Relaxed);
    assert!(parser.push_line("from users").is_err());
    assert!(parser.finish().is_err());
    assert!(parse_runtime_sql_records_with_cache("", &mut cache, &cancel).is_err());
}

/// 动态领取会乱序完成，但成功、文件名失败和读取失败均按原始来源位置稳定合并。
#[test]
fn dynamic_file_dispatch_preserves_order_and_failure_records() {
    let dir = crate::config::paths::isolated_test_dir("runtime-dynamic-dispatch");
    fs::create_dir_all(&dir).expect("应创建动态分配测试目录");
    let mut targets = Vec::new();
    for index in 0..MAX_RUNTIME_PARSE_WORKERS * 3 {
        let label = if index == 4 {
            "invalid.log".to_string()
        } else {
            format!("100&user&_api_{index}&{}&0&0.log", 1000 + index)
        };
        let path = dir.join(&label);
        if index != 7 {
            let sql = "1 0 0 0 0 select 1\n".repeat(if index == 0 { 500 } else { 1 });
            fs::write(&path, sql).expect("应写入动态分配测试日志");
        }
        targets.push(RuntimeAnalysisTarget {
            source_id: SourceId(index),
            label,
            path: path.display().to_string(),
            location: SourceLocation::LocalPath(path),
            kind: RuntimeAnalysisTargetKind::File,
        });
    }
    let result = analyze_runtime_targets(
        targets,
        "UTF-8".into(),
        LoaderConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(result.total_files, MAX_RUNTIME_PARSE_WORKERS * 3);
    assert_eq!(
        result
            .requests
            .iter()
            .map(|request| request.source_id)
            .collect::<Vec<_>>(),
        (0..MAX_RUNTIME_PARSE_WORKERS * 3)
            .filter(|index| ![4, 7].contains(index))
            .map(SourceId)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        result
            .skipped_files
            .iter()
            .map(|file| file.source_id)
            .collect::<Vec<_>>(),
        vec![SourceId(4), SourceId(7)]
    );
    assert_eq!(result.requests[0].sql_records.len(), 500);
    assert!(
        result
            .requests
            .iter()
            .enumerate()
            .all(|(index, request)| request.index == index)
    );
    fs::remove_dir_all(dir).expect("应清理动态分配测试目录");
}
