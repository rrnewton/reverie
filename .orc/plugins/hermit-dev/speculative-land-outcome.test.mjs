#!/usr/bin/env node
// Durable regression test for the speculative-land tick's OUTCOME CLASSIFICATION
// and its ALERT-DEDUPE INVARIANT.
//
// Why this file exists as a standalone harness rather than an import: the logic
// under test lives inside a `wf.loop` closure in index.ts, which cannot be
// imported without an `orc` host. Rather than restructure production code to be
// testable (scope this task does not have), the decision table is transcribed
// here and pinned. That transcription is the test's known weakness, so it is
// guarded: `test_transcription_matches_index_ts` greps the real file for each
// expression below and fails if index.ts drifts from what is pinned here.
//
// Run: node .orc/plugins/hermit-dev/speculative-land-outcome.test.mjs
// Exits 0 on pass, 1 on failure, and prints counts on both sides.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const HERE = dirname(fileURLToPath(import.meta.url));
const INDEX_TS = join(HERE, "index.ts");

const SPECULATIVE_LAND_TIMEOUT_SEC = 240;

// --- the decision table, transcribed verbatim from index.ts ------------------

function classify({ timedOut, exitCode, stdout }) {
  const outcome = timedOut ? "timeout" : (exitCode > 1 ? "error" : "ok");
  const remediationRequired =
    exitCode === 2 && stdout.includes("state=remediation-required");
  const pollFailed = exitCode > 1 && !remediationRequired;
  return { outcome, remediationRequired, pollFailed };
}

function stableAlertSignature(report) {
  const lines = report.split("\n");
  if (lines.includes("watch_status=no-result")) {
    const stablePrefixes = [
      "state=",
      "verdict=",
      "watch_status=",
      "watch_verdict=",
      "watch_planned=",
      "watch_stopped_because=",
    ];
    return lines.filter((line) =>
      stablePrefixes.some((prefix) => line.startsWith(prefix))
    ).join("\n");
  }
  return lines.filter((line) => !line.startsWith("COST ")).join("\n");
}

function speculativeLandTimeoutReport(elapsedMs) {
  const boundMs = SPECULATIVE_LAND_TIMEOUT_SEC * 1000;
  return [
    "WATCH OBLIGATIONS: NO-RESULT checked=0 planned=unknown " +
      "elapsed_ms=" + elapsedMs + " bound_ms=" + boundMs,
    "state=no-result",
    "verdict=NO-RESULT",
    "watch_status=no-result",
    "watch_verdict=NO-RESULT",
    "watch_planned=unknown",
    "watch_checked=0",
    "watch_attempted=unknown",
    "watch_timed_out=true",
    "watch_stopped_because=outer-timeout",
    "elapsed_ms=" + elapsedMs,
    "bound_ms=" + boundMs,
    "summary=NO-RESULT: the speculative-land obligation watcher exceeded " +
      "its " + boundMs + "ms outer bound before reporting any obligation. " +
      "No completed obligation check was reported, so checked and clear was " +
      "not established.",
  ].join("\n");
}

function timingLine({ outcome, elapsedMs, exitCode }) {
  return "outcome=" + outcome + " elapsed_ms=" + elapsedMs +
    " bound_ms=" + (SPECULATIVE_LAND_TIMEOUT_SEC * 1000) +
    " exit_code=" + exitCode;
}

// --- harness -----------------------------------------------------------------

let passed = 0;
const failures = [];
function check(name, cond, detail = "") {
  if (cond) { passed++; return; }
  failures.push(name + (detail ? "  -- " + detail : ""));
}

// === 1. The steady state must stay silent ====================================
// Live ticks return exit 1 with gate semantics ("checked=5 unresolved=5
// remediation_required=0"). Exit 1 is NOT a failure here. If this ever classifies
// as error/pollFailed, the fleet gets an alert every 15 seconds.
{
  const r = classify({ timedOut: false, exitCode: 1, stdout: "checked=5 unresolved=5 remediation_required=0" });
  check("gate exit 1 => outcome=ok", r.outcome === "ok", `got ${r.outcome}`);
  check("gate exit 1 => not pollFailed", r.pollFailed === false);
  check("gate exit 1 => not remediationRequired", r.remediationRequired === false);
}
{
  const r = classify({ timedOut: false, exitCode: 0, stdout: "" });
  check("exit 0 => outcome=ok", r.outcome === "ok");
  check("exit 0 => not pollFailed", r.pollFailed === false);
}

// === 2. THE FIX: a timeout becomes a typed value, not an escaping throw =======
{
  const stdout = speculativeLandTimeoutReport(240011);
  const r = classify({ timedOut: true, exitCode: 3, stdout });
  check("timeout => outcome=timeout", r.outcome === "timeout", `got ${r.outcome}`);
  check("timeout => routes to the EXISTING pollFailed branch", r.pollFailed === true);
  check("timeout => never mistaken for remediation", r.remediationRequired === false);
  check("timeout explicitly says NO-RESULT", stdout.includes("watch_verdict=NO-RESULT"));
  check("timeout explicitly says zero checked", stdout.includes("watch_checked=0"));
  check("timeout names the outer bound", stdout.includes("watch_stopped_because=outer-timeout"));
  check("timeout refuses to claim clear", stdout.includes("checked and clear was not established"));
}

// === 3. Exit 3 must not collide with remediation's exit 2 ====================
// The whole reason 3 was chosen over 2. If a timeout could present as
// remediation-required it would send a wake to hermit-lander for a fault that
// never happened.
{
  const rem = classify({ timedOut: false, exitCode: 2, stdout: "state=remediation-required\nids=abc" });
  check("exit 2 + marker => remediationRequired", rem.remediationRequired === true);
  check("exit 2 + marker => not pollFailed", rem.pollFailed === false);
  const bare = classify({ timedOut: false, exitCode: 2, stdout: "no marker here" });
  check("exit 2 WITHOUT marker => pollFailed, not remediation", bare.pollFailed === true && bare.remediationRequired === false);
}

// === 4. THE DEDUPE INVARIANT (the defect this test exists to prevent) ========
// The alert signature must NOT contain elapsed_ms. If it did, every repeated
// timeout would produce a fresh signature, defeat the kvGet/kvSet dedupe, and
// turn one repeating fault into an alert storm at the 15s tick interval.
{
  const reportA = speculativeLandTimeoutReport(240011);
  const reportB = speculativeLandTimeoutReport(240987);
  check("timeout reports retain their actual durations", reportA !== reportB);
  check(
    "repeated outer timeouts share a stable signature",
    stableAlertSignature(reportA) === stableAlertSignature(reportB),
  );

  const partialA = [
    "state=no-result",
    "verdict=NO-RESULT",
    "watch_status=no-result",
    "watch_verdict=NO-RESULT",
    "watch_planned=16",
    "watch_checked=3",
    "watch_rotation_next=ob-3",
    "watch_stopped_because=budget",
    "elapsed_ms=20001",
    "watch_unchecked_ids=ob-3,ob-4",
  ].join("\n");
  const partialB = [
    "state=no-result",
    "verdict=NO-RESULT",
    "watch_status=no-result",
    "watch_verdict=NO-RESULT",
    "watch_planned=16",
    "watch_checked=4",
    "watch_rotation_next=ob-7",
    "watch_stopped_because=budget",
    "elapsed_ms=20999",
    "watch_unchecked_ids=ob-7,ob-8",
  ].join("\n");
  check(
    "rotation progress does not repeat the repair instruction every cycle",
    stableAlertSignature(partialA) === stableAlertSignature(partialB),
  );
  check(
    "a changed stop reason produces a new alert",
    stableAlertSignature(partialA) !== stableAlertSignature(
      partialB.replace("watch_stopped_because=budget", "watch_stopped_because=github-quota"),
    ),
  );
  check(
    "a changed planned count produces a new alert",
    stableAlertSignature(partialA) !== stableAlertSignature(
      partialB.replace("watch_planned=16", "watch_planned=17"),
    ),
  );
}

// === 5. The timing line carries its own denominator ==========================
{
  const line = timingLine({ outcome: "timeout", elapsedMs: 240011, exitCode: 3 });
  check("timing names the bound it was measured against", line.includes("bound_ms=240000"));
  check("timing names the duration", line.includes("elapsed_ms=240011"));
  check("timing names the outcome", line.includes("outcome=timeout"));
  check("timing names the exit code", line.includes("exit_code=3"));
}

// === 6. TRANSCRIPTION GUARD: index.ts must still match what is pinned above ==
// Without this, index.ts could change and every assertion above would keep
// passing against a stale copy -- a test that verifies nothing.
{
  let src = "";
  try { src = readFileSync(INDEX_TS, "utf8"); } catch (e) { /* reported below */ }
  check("index.ts is readable", src.length > 0);
  const required = [
    'const outcome = timedOut ? "timeout" : (exitCode > 1 ? "error" : "ok");',
    "const pollFailed = exitCode > 1 && !remediationRequired;",
    "const SPECULATIVE_LAND_TIMEOUT_SEC = 240;",
    "timeoutSec: SPECULATIVE_LAND_TIMEOUT_SEC,",
    'exitCode: 3,',
  ];
  for (const needle of required) {
    check("index.ts still contains: " + needle, src.includes(needle));
  }
  // The signature function must keep the typed NO-RESULT reason and planned
  // count while excluding progress and timing fields.
  check(
    "stableAlertSignature recognizes typed NO-RESULT",
    src.includes('lines.includes("watch_status=no-result")'),
  );
  check(
    "stableAlertSignature retains the stop reason",
    src.includes('"watch_stopped_because="'),
  );
  check(
    "outer timeout constructs a typed report",
    src.includes("result.stdout = speculativeLandTimeoutReport(elapsedMs);"),
  );
  check(
    "outer timeout reports zero checked",
    src.includes('"watch_checked=0"'),
  );
  // `timing` must reach the wake BODY and must NOT be spliced into `report`.
  check(
    "timing is appended to the wake body",
    src.includes('"\\n" + timing +'),
  );
  check(
    "report is still built from stdout/stderr only (timing NOT folded in)",
    src.includes('const report = [stdout, stderr].filter(Boolean).join("\\n");'),
  );
  check(
    "watcher bypasses the rust-script/Cargo serialization point",
    src.includes("./ci-hub/bin/speculative-land watch --once --gate"),
  );
  check(
    "watcher does not invoke the Rust front door",
    !src.includes("./ci-hub/ci-hub watch-obligations --once --gate"),
  );
}

// --- report -------------------------------------------------------------------

const total = passed + failures.length;
if (failures.length) {
  console.error(`FAIL  ${failures.length} of ${total} checks failed:`);
  for (const f of failures) console.error("  - " + f);
  process.exit(1);
}
console.log(`ok  ${passed}/${total} checks passed`);
