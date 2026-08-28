def key:
  [.cell.test, .cell.mode, .cell.backend, .cell.lane, .cell.category, .repetition];

.rows
| sort_by(key + [.attempt])
| group_by(key)
| map(max_by(.attempt))[]
| [
    $batch,
    .cell.test,
    .cell.mode,
    .cell.backend,
    .cell.lane,
    .cell.category,
    .repetition,
    .attempt,
    .result,
    .outcome,
    (.error_kind // ""),
    ((.reason // "") | gsub("[\\t\\r\\n]"; " ")),
    .timeout_proven_by_runner_and_attempt_marker,
    .oom_proven_by_runner_and_attempt_marker
  ]
| @tsv
