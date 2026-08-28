def key:
  [.cell.test, .cell.mode, .cell.backend, .cell.lane, .cell.category, .repetition];

def count_result($name):
  map(select(.result == $name)) | length;

(.rows
 | sort_by(key + [.attempt])
 | group_by(key)
 | map(max_by(.attempt)))
| group_by([.cell.test, .cell.mode, .cell.backend, .cell.lane, .cell.category])[]
| . as $rows
| ($rows | map(.result) | unique) as $results
| [
    $batch,
    $rows[0].cell.test,
    $rows[0].cell.mode,
    $rows[0].cell.backend,
    $rows[0].cell.lane,
    $rows[0].cell.category,
    ($rows | count_result("pass")),
    ($rows | count_result("infrastructure-error")),
    ($rows | count_result("crash-error")),
    ($rows | count_result("determinism-failure")),
    ($rows | map(select(.timeout_proven_by_runner_and_attempt_marker)) | length),
    ($rows | map(select(.oom_proven_by_runner_and_attempt_marker)) | length),
    ($rows | map(select(.attempt > 1)) | length),
    ($rows | map(.attempt) | add),
    (if ($results | length) == 1 then "uniform " + $results[0] else "varied" end)
  ]
| @tsv
