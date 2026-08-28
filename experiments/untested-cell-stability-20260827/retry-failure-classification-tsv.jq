def key:
  [.cell.test, .cell.mode, .cell.backend, .cell.lane, .cell.category, .repetition];

def failure_class:
  if .result == "pass" then null
  elif (.result == "determinism-failure"
        or .result == "parity-failure"
        or .result == "replay-failure"
        or .result == "crash-error") then "product_failure"
  elif (.result == "timeout" or .result == "oom") then "no_result"
  else error("retry-failure-classification: unsupported retained result \(.result|tojson)")
  end;

def text:
  if . == null then "null" else tostring end;

. as $document
| ($document.rows
   | sort_by(key + [.attempt])
   | group_by(key)) as $groups
| ($groups | map(select(length > 1))) as $retries
| if $document.schema != 4 then
    error("retry-failure-classification: expected source schema 4")
  elif ($document.rows | length) != 873 then
    error("retry-failure-classification: expected 873 source attempts")
  elif ($groups | length) != 850 then
    error("retry-failure-classification: expected 850 result groups")
  elif ($retries | length) != 23 then
    error("retry-failure-classification: expected 23 retry groups")
  else $document
  end
| ($retries
   | map(
       if length != 2 or .[0].attempt != 1 or .[1].attempt != 2 then
         error("retry-failure-classification: retry group does not contain exact attempts 1 and 2")
       elif .[0].result != "determinism-failure" then
         error("retry-failure-classification: first attempt is not determinism-failure")
       elif .[0].error_kind != null or .[1].error_kind != null then
         error("retry-failure-classification: retained retry unexpectedly carries error_kind")
       elif .[0].reason != "canonical verification did not match: verified=false verdict=diverged bitwise_parity=false" then
         error("retry-failure-classification: first attempt lost the canonical divergence reason")
       else .
       end
       | [
           .[0].cell.test,
           .[0].cell.mode,
           .[0].cell.backend,
           .[0].cell.lane,
           .[0].cell.category,
           .[0].repetition,
           .[0].result,
           (.[0] | failure_class),
           .[0].error_kind,
           .[0].reason,
           .[1].result,
           (.[1] | failure_class),
           .[1].error_kind,
           .[1].reason,
           .[1].outcome,
           (.[1].result == "pass"),
           (.[0].result != .[1].result)
         ])) as $projection
| ([
    "test",
    "mode",
    "backend",
    "lane",
    "category",
    "repetition",
    "first_attempt_result",
    "first_attempt_failure_class",
    "first_attempt_error_kind",
    "first_attempt_reason",
    "second_attempt_result",
    "second_attempt_failure_class",
    "second_attempt_error_kind",
    "second_attempt_reason",
    "terminal_outcome",
    "recovered",
    "result_changed"
  ] | @tsv),
  ($projection[] | map(text) | @tsv)
