#!/usr/bin/env node
// Regression test for the owner-directed removal of the recurring
// speculative-land obligation workflow from the hermit-dev ORC plugin.

import { existsSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const indexSource = readFileSync(join(here, "index.ts"), "utf8");
const root = join(here, "../../..");
const ciHubSource = readFileSync(join(root, "ci-hub/ci-hub.rs"), "utf8");
const protocolSource = readFileSync(
  join(root, "ci-hub/remediation/protocol.py"),
  "utf8",
);

let passed = 0;
const failures = [];
function check(name, condition) {
  if (condition) {
    passed += 1;
  } else {
    failures.push(name);
  }
}

for (const removed of [
  "SPECULATIVE_LAND_SCRIPT_NAME",
  "SPECULATIVE_LAND_COMMAND",
  "SPECULATIVE_LAND_WAKE_SCRIPT_NAME",
  "SPECULATIVE_LAND_WAKE_COMMAND",
  "SPECULATIVE_LAND_INTERVAL_MS",
  "SPECULATIVE_LAND_TIMEOUT_SEC",
  "SPECULATIVE_LAND_ALERT_CACHE_KEY",
  "speculativeLandRemediationHeartbeat",
  "land_and_arm.py recover --observe-timeout 5",
  "speculative-land watch --once --gate",
  "record-obligation-wake",
]) {
  check("index.ts does not contain " + removed, !indexSource.includes(removed));
}

check(
  "the removed workflow is retained only for startup cleanup",
  (indexSource.match(/hermit-dev-speculative-land-remediation-v1/g) || []).length === 1 &&
    indexSource.includes("const LEGACY_SPECULATIVE_LAND_WORKFLOW_NAME =") &&
    indexSource.includes("LEGACY_SPECULATIVE_LAND_WORKFLOW_NAME,"),
);
for (const removedPath of [
  "ci-hub/bin/speculative-land",
  "ci-hub/history/obligations.py",
  "ci-hub/remediation/decision-waits.json",
  "ci-hub/remediation/land_and_arm.py",
]) {
  check(removedPath + " is deleted", !existsSync(join(root, removedPath)));
}
for (const removedCommand of [
  "ArmLand",
  "ObligationsArgs",
  "InheritObligations",
  "RecordObligationWake",
  "WatchObligations",
  "CancelWatcher",
  "AuthorizeRevert",
  "ResolveObligation",
  "CI_HUB_OBLIGATIONS_STORE",
]) {
  check("ci-hub.rs does not contain " + removedCommand, !ciHubSource.includes(removedCommand));
}
for (const removedImplementation of [
  "import obligations",
  "def arm(",
  "def watch(",
  "def record_wake_sent(",
  "def inherit_actionable(",
  "def resolve_obligation(",
  "def cancel_watcher(",
  "def _local_run(",
]) {
  check(
    "protocol.py does not contain " + removedImplementation,
    !protocolSource.includes(removedImplementation),
  );
}
const total = passed + failures.length;
if (failures.length > 0) {
  console.error(`FAIL  ${failures.length} of ${total} checks failed:`);
  for (const failure of failures) console.error("  - " + failure);
  process.exit(1);
}
console.log(`ok  ${passed}/${total} checks passed`);
