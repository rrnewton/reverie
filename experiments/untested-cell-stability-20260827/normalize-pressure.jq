def run_key:
  [.cell.test, .cell.mode, .cell.backend, .cell.lane, .cell.category, .repetition];

def final_rows:
  .rows
  | sort_by(run_key + [.attempt])
  | group_by(run_key)
  | map(max_by(.attempt));

def result_counts:
  sort
  | group_by(.)
  | map({result: .[0], count: length});

. as $summary
| (final_rows) as $finals
| ($finals
   | group_by([.cell.test, .cell.mode, .cell.backend, .cell.lane, .cell.category])
   | map({
       cell: .[0].cell,
       result_counts: (map(.result) | result_counts),
       distinct_results: (map(.result) | unique | length),
       retried_repetitions: (map(select(.attempt > 1)) | length),
       extra_attempts: (map(.attempt - 1) | add),
       final_timeouts: (map(select(.timeout_proven_by_runner_and_attempt_marker)) | length),
       final_ooms: (map(select(.oom_proven_by_runner_and_attempt_marker)) | length)
     })) as $cells
| {
    schema: $summary.schema,
    run_id_prefix: $summary.run_id_prefix,
    hermit_sha: $summary.hermit_sha,
    detcore_tree: $summary.detcore_tree,
    source_tree_dirty: $summary.source_tree_dirty,
    eligible_cells: $summary.eligible_cells,
    selected_cells: $summary.selected_cells,
    repetitions: $summary.repetitions,
    jobs: $summary.jobs,
    cell_timeout_seconds: $summary.cell_timeout_seconds,
    repeated_result: $summary.repeated_result,
    summary_rows: ($summary.rows | length),
    final_results: ($finals | length),
    execution_attempts: ($finals | map(.attempt) | add),
    retried_repetitions: ($finals | map(select(.attempt > 1)) | length),
    result_counts: ($finals | map(.result) | result_counts),
    final_timeouts: ($finals | map(select(.timeout_proven_by_runner_and_attempt_marker)) | length),
    final_ooms: ($finals | map(select(.oom_proven_by_runner_and_attempt_marker)) | length),
    uniform_pass_cells: ($cells | map(select(.result_counts == [{result:"pass",count:10}])) | length),
    uniform_infrastructure_error_cells: ($cells | map(select(.result_counts == [{result:"infrastructure-error",count:10}])) | length),
    uniform_crash_error_cells: ($cells | map(select(.result_counts == [{result:"crash-error",count:10}])) | length),
    uniform_determinism_failure_cells: ($cells | map(select(.result_counts == [{result:"determinism-failure",count:10}])) | length),
    varied_cells: ($cells | map(select(.distinct_results > 1))),
    cells: $cells
  }
