#!/usr/bin/env node
// Durable regression test for REGISTERED-SCRIPT ARGUMENT PASSING and the
// Hard-Invariant-16 death gate that rides on it.
//
// WHY THIS FILE EXISTS. On 2026-08-10 the mandated pre-replacement probe was
// invoked with an empty `$1` and answered
// `{"agent": "", "state": "verified-dead", "may_replace": true}` for EVERY
// query, including agents that were demonstrably alive. Nothing was wrong with
// the probe's scan -- it swept 265 processes, carried `blind_spot: 0`, and saw
// another agent alive. The verdict was simply about the empty name instead of
// about the subject, and an empty name matches nothing, so "absence" was
// established vacuously. A fail-OPEN safety gate wired to close+respawn.
//
// The diagnosis that followed was wrong in an instructive way, and this file
// pins the correction so it is not re-derived: the surface does NOT drop
// arguments. The inert echo checks below bracket the live registered-script
// binding with multiple distinctive values.
// What actually breaks is calling `orc.callScriptInternal` by hand, whose
// `argsJson` parameter is a JSON STRING -- pass an array and the dispatcher
// rejects it, pass a bare name and NO positionals bind.
//
// Like its sibling harnesses, the logic under test lives inside index.ts and
// cannot be imported without an `orc` host. The SHELL STRING is not
// transcribed, though -- it is extracted from index.ts at run time, so the
// command actually executed here is the one that ships. The decision table for
// the death-gate wrapper IS transcribed, and that transcription is the known
// weakness, so `drift guard` below greps index.ts for each behaviour and fails
// if the two drift apart.
//
// Nothing here kills, replaces, or authorizes anything: the echo script prints
// its own argv, and the probe only reads /proc.
//
// Run: node .orc/plugins/hermit-dev/script-arg-passing.test.mjs
// Exits 0 on pass, 1 on failure, and prints counts on both sides.

import { readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const HERE = dirname(fileURLToPath(import.meta.url));
const INDEX_TS = join(HERE, "index.ts");
const SOURCE = readFileSync(INDEX_TS, "utf8");

function sourceLineOf(fragment) {
  const offset = SOURCE.indexOf(fragment);
  if (offset < 0) return -1;
  return SOURCE.slice(0, offset).split("\n").length;
}

let passed = 0;
const failures = [];
function check(name, cond, detail = "") {
  if (cond) { passed++; return; }
  failures.push(name + (detail ? "  -- " + detail : ""));
}

check(
  "retired operational workflow source stays absent",
  sourceLineOf("export async function operationalHealthHeartbeat") === -1,
  String(sourceLineOf("export async function operationalHealthHeartbeat")),
);
check(
  "policy reload remains the sole declared heartbeat source",
  sourceLineOf("export async function policyReloadHeartbeat") > 0 &&
    (SOURCE.match(/export async function \w+Heartbeat/g) || []).length === 1,
  String(sourceLineOf("export async function policyReloadHeartbeat")),
);

// --- bind to the REAL literal, not a copy of it ------------------------------
// Extracting and evaluating the shipped concatenation removes the usual
// transcription gap for the one string whose exact bytes are the whole point.
function literalFromSource(constName) {
  const match = SOURCE.match(
    new RegExp("const " + constName + " =\\s*([\\s\\S]*?);\\n"),
  );
  if (!match) throw new Error("cannot find " + constName + " in index.ts");
  const expr = match[1];
  if (!/^[\s'"`+\w\-.,:{}()[\]$@#%/\\!=><*;|&~^?]*$/.test(expr)) {
    throw new Error(constName + " is not a plain string expression: " + expr);
  }
  return Function('"use strict"; return (' + expr + ");")();
}

const ARGV_ECHO_COMMAND = literalFromSource("ARGV_ECHO_COMMAND");

check(
  "argv echo uses the system Python without the allocator warning",
  ARGV_ECHO_COMMAND.startsWith("/usr/bin/python3 "),
  ARGV_ECHO_COMMAND,
);
check(
  "argv echo contains no braces for ORC to substitute",
  !ARGV_ECHO_COMMAND.includes("{") && !ARGV_ECHO_COMMAND.includes("}"),
  ARGV_ECHO_COMMAND,
);

// Run the shipped command the way a correct runtime does: `sh -c CMD $0 args…`.
function runEcho(args, { offByOne = false } = {}) {
  // offByOne models the classic mis-binding, where the first argument lands in
  // `$0` and `$1` is empty. It is here so the test can PROVE the diagnostic
  // distinguishes the two, rather than only that it works when things are fine.
  const argv = offByOne
    ? ["-c", ARGV_ECHO_COMMAND, ...args]
    : ["-c", ARGV_ECHO_COMMAND, "sh", ...args];
  const out = execFileSync("sh", argv, { encoding: "utf8" });
  return JSON.parse(out);
}

// === 1. POSITIVE: the exact arguments must arrive ============================
// The payload carries a space and an empty string on purpose: a binder that
// re-splits on whitespace or drops empties would otherwise look correct.
{
  const sent = ["alpha", "two words", "", "4"];
  const got = runEcho(sent);
  check(
    "arguments arrive exactly",
    Array.isArray(got.argv) && got.argv.length === sent.length &&
      sent.every((v, i) => got.argv[i] === v),
    "sent " + JSON.stringify(sent) + " got " + JSON.stringify(got.argv),
  );
  check(
    "an embedded space is one argument, not two",
    got.argv[1] === "two words",
    JSON.stringify(got.argv[1]),
  );
  check(
    "an empty argument is preserved rather than dropped",
    got.argv[2] === "" && got.argv.length === 4,
    JSON.stringify(got.argv),
  );
  check(
    "$0 is the shell name, so positionals are not shifted",
    got.dollar_zero === "sh" && got.dollar_zero !== sent[0],
    "dollar_zero=" + JSON.stringify(got.dollar_zero),
  );
}

// === 2. NEGATIVE: no arguments must yield genuinely empty argv ===============
// Without this, "arguments arrive" could be satisfied by a stub that echoes a
// canned payload no matter what it is handed.
{
  const got = runEcho([]);
  check(
    "no arguments yields empty argv",
    Array.isArray(got.argv) && got.argv.length === 0,
    JSON.stringify(got.argv),
  );
}

// === 3. The diagnostic must FIRE on the mis-binding it exists to detect ======
// This is the bracket that makes check 1 meaningful: a detector that cannot
// report the bad case is not evidence when it reports the good one.
{
  const sent = ["alpha", "two words"];
  const got = runEcho(sent, { offByOne: true });
  check(
    "off-by-one binding is detected via $0",
    got.dollar_zero === sent[0],
    "dollar_zero=" + JSON.stringify(got.dollar_zero) + " (expected the first " +
      "argument to have landed in $0 under this convention)",
  );
  check(
    "off-by-one binding visibly loses an argument",
    got.argv.length === sent.length - 1,
    JSON.stringify(got.argv),
  );
}

// === 4. The death-gate decision table ========================================
// Transcribed from hermitVerifyAgentDeath in index.ts; guarded by section 6.
function deathGateVerdict({ agent, exitCode, stdout = "", stderr = "" }) {
  if (typeof agent !== "string" || agent.trim().length === 0) {
    return { may_replace: false, rc: 2, why: "blank-subject" };
  }
  const raw = String(stdout || "").trim() || String(stderr || "").trim();
  if (!raw) return { may_replace: false, rc: 2, why: "no-output" };
  let parsed;
  try { parsed = JSON.parse(raw); } catch {
    return { may_replace: false, rc: 2, why: "unparseable" };
  }
  if (parsed.agent !== agent) {
    return { may_replace: false, rc: 2, why: "subject-mismatch" };
  }
  const permitted = Number(exitCode) === 0 &&
    parsed.state === "verified-dead" && parsed.may_replace === true;
  return { may_replace: permitted, rc: Number(exitCode), why: "decided" };
}

const DEAD_BODY = JSON.stringify({
  agent: "ghost", state: "verified-dead", may_replace: true, live_pids: [],
});

// PERMIT side -- the gate must still allow genuine self-healing. A safeguard
// that refuses everything is its own failure mode, and the easier one to ship.
{
  const v = deathGateVerdict({ agent: "ghost", exitCode: 0, stdout: DEAD_BODY });
  check("a genuine death is still replaceable", v.may_replace === true, v.why);
}

// REFUSE side.
{
  for (const blank of ["", " ", "\t", "\n"]) {
    const v = deathGateVerdict({ agent: blank, exitCode: 0, stdout: DEAD_BODY });
    check(
      "blank subject refuses: " + JSON.stringify(blank),
      v.may_replace === false && v.rc === 2,
      v.why,
    );
  }
  check(
    "a non-string subject refuses",
    deathGateVerdict({ agent: undefined, exitCode: 0, stdout: DEAD_BODY })
      .may_replace === false,
  );

  // THE ORIGINAL BUG, at the caller. The probe answers about "" while the
  // caller asked about a live agent; without the identity comparison this reads
  // as a clean permit.
  {
    const v = deathGateVerdict({
      agent: "codex-tgaudit",
      exitCode: 0,
      stdout: JSON.stringify({
        agent: "", state: "verified-dead", may_replace: true, live_pids: [],
      }),
    });
    check(
      "a verdict about a DIFFERENT subject refuses",
      v.may_replace === false && v.why === "subject-mismatch",
      v.why,
    );
  }

  // The probe writes refusals to STDERR and permits to STDOUT. A wrapper that
  // read stdout only would see an empty body for every ALIVE verdict.
  {
    const alive = JSON.stringify({
      agent: "claude-1", state: "alive", may_replace: false, live_pids: [7, 9],
    });
    const v = deathGateVerdict({ agent: "claude-1", exitCode: 1, stderr: alive });
    check(
      "an ALIVE verdict on stderr is read and refuses",
      v.may_replace === false && v.rc === 1 && v.why === "decided",
      v.why,
    );
  }

  check(
    "empty output refuses",
    deathGateVerdict({ agent: "ghost", exitCode: 0 }).may_replace === false,
  );
  check(
    "non-JSON output refuses",
    deathGateVerdict({ agent: "ghost", exitCode: 0, stdout: "boom" })
      .may_replace === false,
  );
  // Disagreement between the exit code and the body is never resolved in
  // favour of the permit.
  check(
    "exit code and body must agree",
    deathGateVerdict({ agent: "ghost", exitCode: 2, stdout: DEAD_BODY })
      .may_replace === false,
  );
  check(
    "a non-dead state with exit 0 refuses",
    deathGateVerdict({
      agent: "ghost",
      exitCode: 0,
      stdout: JSON.stringify({
        agent: "ghost", state: "unverifiable", may_replace: false,
      }),
    }).may_replace === false,
  );
}

// === 5. The registered probe command must keep the working shape =============
{
  const probeCmd = SOURCE.match(
    /const AGENT_LIVENESS_COMMAND =\s*([\s\S]*?);\n/,
  );
  check("AGENT_LIVENESS_COMMAND is present", Boolean(probeCmd));
  const text = probeCmd ? probeCmd[1] : "";
  check(
    'the probe command passes "$1"',
    text.includes('"$1"'),
    text,
  );
  // The registered-script runtime substitutes brace-delimited text before
  // invoking the shell. This probe needs only shell positionals.
  check(
    "the probe command contains no braces for ORC to substitute",
    !text.includes("{") && !text.includes("}"),
    text,
  );
  check(
    "the probe command asks for --json",
    text.includes("--json"),
    text,
  );
  check(
    "the probe uses the system Python without the allocator warning",
    text.includes("/usr/bin/python3 "),
    text,
  );
}

// === 6. Drift guard: index.ts must still do what section 4 pins ==============
{
  const required = [
    ["exposes the documented top-level wrapper",
      /orc\.exposeFunction\(\s*AGENT_LIVENESS_SCRIPT_NAME,\s*\n?\s*hermitVerifyAgentDeath/],
    ["registers the inert argv-echo script",
      /orc\.registerScript\(ARGV_ECHO_SCRIPT_NAME/],
    ["marshals args as a JSON string on the fallback path",
      /callScriptInternal\(name, JSON\.stringify\(args\)\)/],
    ["prefers the orc.scripts proxy",
      /const scripts = orc\.scripts/],
    ["cleans the proxy result before parsing",
      /return cleanScriptResult\(result\);/],
    ["cleans the fallback result before parsing",
      /return cleanScriptResult\(\s*\n?\s*\(unwrapped === undefined \? response : unwrapped\)/],
    ["refuses a blank subject",
      /agent\.trim\(\)\.length === 0/],
    ["compares the answered subject to the asked subject",
      /parsed\.agent !== agent/],
    ["reads stderr when stdout is empty",
      /String\(result\.stdout \|\| ""\)\.trim\(\) \|\|\s*\n?\s*String\(result\.stderr \|\| ""\)\.trim\(\)/],
    ["permits only on unanimous agreement",
      /Number\(result\.exitCode\) === 0 &&\s*\n?\s*parsed\.state === "verified-dead" &&\s*\n?\s*parsed\.may_replace === true/],
  ];
  for (const [name, pattern] of required) {
    check("index.ts " + name, pattern.test(SOURCE), String(pattern));
  }
}

// === 7. Policy context injection: cache state is not context authority =======
// Evaluate the ACTUAL helper body from index.ts with an inert ORC stub. This
// catches the production defect that a source-cache hit returned "activated"
// without calling either activateSkill or interjection after runtime resume.
function sourceBetween(startMarker, endMarker) {
  const start = SOURCE.indexOf(startMarker);
  const end = SOURCE.indexOf(endMarker, start);
  if (start < 0 || end < 0) {
    throw new Error("cannot extract " + startMarker + " from index.ts");
  }
  return SOURCE.slice(start, end);
}

// Execute the shipped retirement reconciler with an in-memory registry. This
// brackets benign absence, successful removal, kill failure, and a kill that
// reports success without actually removing the competing producer.
function compileWorkflowRetirement(functionSource, orcStub, hasKill = true) {
  const javascript = functionSource
    .replace("requested: readonly string[]", "requested")
    .replace("): Promise<WorkflowRetirementResult>", ")")
    .replace("const result: WorkflowRetirementResult =", "const result =")
    .replace("let before: unknown;", "let before;")
    .replace("let after: unknown;", "let after;");
  const registeredNames = (entries) => new Set(
    (Array.isArray(entries) ? entries : [])
      .map((entry) => String(entry?.name || ""))
      .filter((name) => name.length > 0),
  );
  return new Function(
    "registeredNames",
    "hasOrcSurface",
    "orc",
    "RETIRED_SYSTEMD_REPLACED_WORKFLOW_NAMES",
    javascript + "; return reconcileWorkflowRetirement;",
  )(
    registeredNames,
    (name) => name === "killWorkflow" && hasKill,
    orcStub,
    ["systemd-producer"],
  );
}

async function probeWorkflowRetirement({
  initial = [],
  killError = null,
  sticky = false,
  hasKill = true,
} = {}) {
  const names = new Set(initial);
  const killCalls = [];
  let listCalls = 0;
  const orcStub = {
    listWorkflows: async () => {
      listCalls += 1;
      return [...names].map((name) => ({ name }));
    },
    killWorkflow: async (name) => {
      killCalls.push(name);
      if (killError === name) throw new Error("kill refused for " + name);
      if (!sticky) names.delete(name);
    },
  };
  const retirementSource = sourceBetween(
    "async function reconcileWorkflowRetirement",
    "\n/** Report whether every heartbeat",
  );
  const reconcile = compileWorkflowRetirement(
    retirementSource,
    orcStub,
    hasKill,
  );
  const result = await reconcile(["policy-reload", "systemd-producer"]);
  return { result, killCalls, listCalls };
}

const absentRetirement = await probeWorkflowRetirement();
check(
  "already-absent workflows are benign without a kill call",
  absentRetirement.result.pass === true &&
    absentRetirement.killCalls.length === 0 &&
    absentRetirement.listCalls === 2 &&
    absentRetirement.result.absentBefore.length === 2,
  JSON.stringify(absentRetirement),
);
const successfulRetirement = await probeWorkflowRetirement({
  initial: ["policy-reload", "systemd-producer"],
});
check(
  "present workflows are removed and post-list reconciled",
  successfulRetirement.result.pass === true &&
    successfulRetirement.killCalls.length === 2 &&
    successfulRetirement.listCalls === 2 &&
    successfulRetirement.result.killed.length === 2 &&
    successfulRetirement.result.stillPresent.length === 0,
  JSON.stringify(successfulRetirement),
);
const refusedRetirement = await probeWorkflowRetirement({
  initial: ["systemd-producer"],
  killError: "systemd-producer",
});
check(
  "a real kill error fails and remains attributable",
  refusedRetirement.result.pass === false &&
    refusedRetirement.result.killErrors.length === 1 &&
    refusedRetirement.result.killErrors[0].name === "systemd-producer" &&
    refusedRetirement.result.stillPresentSystemdReplaced[0] ===
      "systemd-producer",
  JSON.stringify(refusedRetirement),
);
const stickyRetirement = await probeWorkflowRetirement({
  initial: ["systemd-producer"],
  sticky: true,
});
check(
  "post-list rejects a kill that leaves the producer registered",
  stickyRetirement.result.pass === false &&
    stickyRetirement.result.killErrors.length === 0 &&
    stickyRetirement.result.stillPresentSystemdReplaced[0] ===
      "systemd-producer",
  JSON.stringify(stickyRetirement),
);
check(
  "retirement receipt is retained in status and startup self-test",
  SOURCE.includes("workflowRetirementAtStartup: lastWorkflowRetirement") &&
    SOURCE.includes("retainWorkflowRetirementCheck(lastWorkflowRetirement)") &&
    SOURCE.includes("checks.push(workflowRetirementCheck(lastWorkflowRetirement))"),
);
check(
  "retirement failure is surfaced and aborts startup",
  SOURCE.includes("HARD WARNING: competing ORC producer retirement failed") &&
    SOURCE.includes('throw new Error("competing workflow retirement failed: "'),
);

function compilePolicyInjectionHelper(functionSource) {
  const javascript = functionSource
    .replace("reason: PolicyInjectionReason", "reason")
    .replace("): Promise<PolicyInjectionReceipt>", ")")
    .replace(
      'let injectionMethod: PolicyInjectionReceipt["injectionMethod"];',
      "let injectionMethod;",
    )
    .replace("const receipt: PolicyInjectionReceipt =", "const receipt =");
  return new Function(
    "resolveSources",
    "registerHermitDevSkill",
    "loadCoordinatorSkill",
    "registerSpeculativeAttackSkill",
    "registerUrgentValidationSkill",
    "orc",
    "POLICY_CACHE_KEY",
    "SKILL_NAME",
    "SPECULATIVE_ATTACK_SKILL_FILE",
    "URGENT_VALIDATION_SKILL_FILE",
    "lastPolicyInjection",
    javascript + "; return injectHermitDevPolicies;",
  );
}

async function probePolicyInjection(functionSource, options = {}) {
  const cached = options.cached ?? true;
  const activationResult = options.activationResult ??
    "Skill 'hermit-dev' already activated.";
  let activateCalls = 0;
  let interjectionCalls = 0;
  let kvSets = 0;
  const policy = "POLICY";
  const orcStub = {
    kvGet: () => cached ? policy : "OLDER POLICY",
    activateSkill: async () => {
      activateCalls += 1;
      if (!activationResult.toLowerCase().includes("already activated")) {
        await orcStub.interjection(policy);
      }
      return activationResult;
    },
    interjection: async (text) => {
      if (text !== policy) throw new Error("wrong policy interjected");
      if (options.failInterjection) throw new Error("interjection refused");
      interjectionCalls += 1;
    },
    kvSet: (key, text) => {
      if (key !== "policy-cache" || text !== policy) {
        throw new Error("wrong policy cached");
      }
      kvSets += 1;
    },
  };
  const helper = compilePolicyInjectionHelper(functionSource)(
    async () => ({ policy, policyPath: "/workspace/AGENTS.md" }),
    () => {},
    async () => {},
    () => {},
    () => {},
    orcStub,
    "policy-cache",
    "hermit-dev",
    "parallel.md",
    "urgent.md",
    null,
  );
  let receipt = null;
  let error = null;
  try {
    receipt = await helper("manual");
  } catch (caught) {
    error = caught;
  }
  return { activateCalls, interjectionCalls, kvSets, receipt, error };
}

function checkPolicyInjected(name, probe, method) {
  check(name + " completes", probe.error === null, String(probe.error));
  check(name + " calls activateSkill once", probe.activateCalls === 1);
  check(name + " interjects once", probe.interjectionCalls === 1);
  check(name + " caches only after injection", probe.kvSets === 1);
  check(
    name + " returns a bound injection receipt",
    probe.receipt?.contextInjected === true &&
      probe.receipt?.injectionMethod === method,
    JSON.stringify(probe.receipt),
  );
}

const policyInjectionSource = sourceBetween(
  "async function injectHermitDevPolicies",
  "\nasync function activateHermitDevPolicies",
);
const cachedInjection = await probePolicyInjection(policyInjectionSource);
checkPolicyInjected("cached runtime resume", cachedInjection, "direct-interjection");

const coldInjection = await probePolicyInjection(policyInjectionSource, {
  cached: false,
  activationResult: "Skill 'hermit-dev' activated.",
});
checkPolicyInjected("cold startup", coldInjection, "activate-skill");

const refusedInjection = await probePolicyInjection(policyInjectionSource, {
  failInterjection: true,
});
check(
  "interjection refusal is a distinct failure",
  String(refusedInjection.error).includes("interjection refused"),
  String(refusedInjection.error),
);
check("interjection refusal does not update the cache", refusedInjection.kvSets === 0);
check("interjection refusal returns no success receipt", refusedInjection.receipt === null);

// Mutation control: restore the old cache-as-authority branch while still
// claiming contextInjected=true. The positive resume bracket above must kill
// it because activateCalls=0 and interjectionCalls=0.
const cacheNeedle =
  "  const policyWasCached = orc.kvGet(POLICY_CACHE_KEY) === sources.policy;\n";
const cacheAuthorityMutation = policyInjectionSource.replace(
  cacheNeedle,
  cacheNeedle +
    "  if (policyWasCached) {\n" +
    "    return { contextInjected: true, injectionMethod: " +
      "\"direct-interjection\", activationResult: \"cached\", " +
      "policyPath: sources.policyPath, policyCharacters: " +
      "sources.policy.length, policyWasCached };\n" +
    "  }\n",
);
const mutatedInjection = await probePolicyInjection(cacheAuthorityMutation);
check(
  "mutation still claims context injection",
  mutatedInjection.receipt?.contextInjected === true,
);
check(
  "mutation is killed by missing activation and interjection",
  mutatedInjection.activateCalls === 0 && mutatedInjection.interjectionCalls === 0,
  JSON.stringify(mutatedInjection),
);

function compilePolicyReloadWorkflow(functionSource, orcStub) {
  const javascript = functionSource
    .replace(
      "export async function policyReloadHeartbeat",
      "async function policyReloadHeartbeat",
    )
    .replace("(wf: WfContext): Promise<void>", "(wf)")
    .replace(" as PolicyInjectionReceipt", "")
    .replace(
      "String((err as { message?: unknown })?.message || err)",
      "String((err && err.message) || err)",
    );
  return new Function(
    "POLICY_RELOAD_INTERVAL_MS",
    "PLUGIN_NAME",
    "orc",
    javascript + "; return policyReloadHeartbeat;",
  )(7_200_000, "hermit-dev", orcStub);
}

async function probePolicyReload(activate, useCollidingFunction = false) {
  const wakeups = [];
  const sleeps = [];
  const activationReasons = [];
  const collisionCalls = [];
  let workflowSource = sourceBetween(
    "export async function policyReloadHeartbeat",
    "\n// --- registered-script invocation",
  );
  if (useCollidingFunction) {
    workflowSource = workflowSource.replace(
      'orc.hermitDevActivate("periodic")',
      'orc[PLUGIN_NAME + ".activate"]("periodic")',
    );
  }
  const workflow = compilePolicyReloadWorkflow(workflowSource, {
    hermitDevActivate: async (reason) => {
      activationReasons.push(reason);
      return JSON.stringify(await activate(reason));
    },
    "hermit-dev.activate": async (reason) => {
      collisionCalls.push(reason);
      return "hermit-dev policies already activated from /workspace/AGENTS.md";
    },
    sendWakeup: async (targets, title, body) => {
      wakeups.push({ targets, title, body });
    },
  });
  await workflow({
    loop: async (body) => body(),
    sleep: async (milliseconds) => sleeps.push(milliseconds),
  });
  return { activationReasons, collisionCalls, sleeps, wakeups };
}

const reloadPassed = await probePolicyReload(async () => ({
  contextInjected: true,
  injectionMethod: "direct-interjection",
  policyCharacters: 123,
  policyPath: "/workspace/AGENTS.md",
}));
check("periodic reload waits two hours", reloadPassed.sleeps[0] === 7_200_000);
check(
  "periodic reload calls the exposed activation with its reason",
  reloadPassed.activationReasons.length === 1 &&
    reloadPassed.activationReasons[0] === "periodic",
  JSON.stringify(reloadPassed.activationReasons),
);
check(
  "periodic reload ignores the colliding compatibility function",
  reloadPassed.collisionCalls.length === 0,
  JSON.stringify(reloadPassed.collisionCalls),
);
check(
  "periodic reload reports completed only after injection",
  reloadPassed.wakeups.length === 1 &&
    reloadPassed.wakeups[0].title === "POLICY RELOAD COMPLETE" &&
    reloadPassed.wakeups[0].body.includes("context_injected=true"),
  JSON.stringify(reloadPassed.wakeups),
);

const reloadRefused = await probePolicyReload(async () => {
  throw new Error("no context delivery");
});
check(
  "periodic reload refusal is distinct from success",
  reloadRefused.wakeups.length === 1 &&
    reloadRefused.wakeups[0].title === "POLICY RELOAD FAILED" &&
    reloadRefused.wakeups[0].body.includes("context_injected=false") &&
    reloadRefused.wakeups[0].body.includes("no context delivery"),
  JSON.stringify(reloadRefused.wakeups),
);

const reloadFalseReceipt = await probePolicyReload(async () => ({
  contextInjected: false,
  injectionMethod: "direct-interjection",
  policyCharacters: 123,
  policyPath: "/workspace/AGENTS.md",
}));
check(
  "periodic reload refuses a receipt without confirmed context injection",
  reloadFalseReceipt.wakeups.length === 1 &&
    reloadFalseReceipt.wakeups[0].title === "POLICY RELOAD FAILED" &&
    reloadFalseReceipt.wakeups[0].body.includes("context_injected=false") &&
    reloadFalseReceipt.wakeups[0].body.includes(
      "policy reload did not confirm context injection",
    ),
  JSON.stringify(reloadFalseReceipt.wakeups),
);

const reloadCollision = await probePolicyReload(
  async () => ({
    contextInjected: true,
    injectionMethod: "direct-interjection",
    policyCharacters: 123,
    policyPath: "/workspace/AGENTS.md",
  }),
  true,
);
check(
  "the stale compatibility function response cannot report success",
  reloadCollision.activationReasons.length === 0 &&
    reloadCollision.collisionCalls.length === 1 &&
    reloadCollision.wakeups.length === 1 &&
    reloadCollision.wakeups[0].title === "POLICY RELOAD FAILED" &&
    reloadCollision.wakeups[0].body.includes("context_injected=false"),
  JSON.stringify(reloadCollision),
);

const registrationSource = sourceBetween(
  "function registerPluginSurface(): void",
  "\n// --- module entry",
);
const skillFunctionsSource = sourceBetween(
  "const SKILL_FUNCTIONS = [",
  "];",
);
const gchatSafeSource = sourceBetween(
  "  // GCHAT SAFE START.",
  "  // GCHAT SAFE END.",
);
const gchatGlobals = {};
new Function("globalThis", '"use strict";\n' + gchatSafeSource)(gchatGlobals);
check(
  "gchatSafe is installed before any ORC registration can fail",
  registrationSource.indexOf('Object.defineProperty(globalThis, "gchatSafe"') >= 0 &&
    registrationSource.indexOf('Object.defineProperty(globalThis, "gchatSafe"') <
      registrationSource.indexOf("orc.exposeFunction"),
);
check("gchatSafe is callable", typeof gchatGlobals.gchatSafe === "function");
check(
  "gchatSafe accepts ordinary text and project acronyms",
  gchatGlobals.gchatSafe(
    "PR and CI use DBT with KVM or QEMU; DAP, MCP, API, CPU, GB, GIB, MB, " +
      "TSC, ABI, URL, JSON, SHA, ORC, EOF, and README are allowed.",
  ).clean === true,
);
const unsafeGchat = gchatGlobals.gchatSafe(
  "PARTLY LANDED, MOSTLY NOT ADDRESSED. DISCIPLINE WORKED, DRAIN DID NOT.",
);
check(
  "gchatSafe reports the caps runs that caused the repeated formatting failure",
  unsafeGchat.clean === false &&
    JSON.stringify(unsafeGchat.offenders) ===
      JSON.stringify([
        "PARTLY LANDED",
        "MOSTLY NOT ADDRESSED",
        "DISCIPLINE WORKED",
        "DRAIN DID NOT",
      ]),
  JSON.stringify(unsafeGchat),
);
check(
  "policy activation is exposed before the reduced-context script guard",
  registrationSource.indexOf('PLUGIN_NAME + ".activate"') >= 0 &&
    registrationSource.indexOf('PLUGIN_NAME + ".activate"') <
      registrationSource.indexOf("orc.registerScript"),
);
for (const name of [
  "hermitDevActivate",
  "hermitDevStatus",
  "hermitDevSelfTest",
]) {
  check(
    "the skill advertises the callable " + name + " property",
    skillFunctionsSource.includes('"' + name + '"'),
  );
  check(
    name + " is exposed before script registration can fail",
    registrationSource.indexOf('orc.exposeFunction("' + name + '"') >= 0 &&
      registrationSource.indexOf('orc.exposeFunction("' + name + '"') <
        registrationSource.indexOf("orc.registerScript"),
  );
}
check(
  "the skill no longer advertises a nonexistent hyphenated namespace",
  !skillFunctionsSource.includes('PLUGIN_NAME + ".'),
);
const surfaceCheckSource = sourceBetween(
  "async function verifyRegisteredSurface",
  "\nfunction hermitDevStatus",
);
const missingNamesSource = sourceBetween(
  "function missingRegisteredNames(",
  "\n\nfunction registeredNames",
)
  .replace(
    "function missingRegisteredNames(\n  entries: unknown,\n  required: string[],\n): string[] {",
    "function missingRegisteredNames(entries, required) {",
  )
  .replace(
    "const row = (entry || {}) as { name?: unknown };",
    "const row = entry || {};",
  );
const missingRegisteredNames = new Function(
  '"use strict";\n' + missingNamesSource + "\nreturn missingRegisteredNames;",
)();

const allocatorWarningSource = sourceBetween(
  "function stripPythonAllocatorWarning(",
  "\n\nfunction cleanScriptResult",
).replace(
  "function stripPythonAllocatorWarning(text: unknown): string {",
  "function stripPythonAllocatorWarning(text) {",
);
const allocatorWarning =
  "<jemalloc>: Invalid conf pair: experimental_infallible_new:true";
const stripPythonAllocatorWarning = new Function(
  "PYTHON_ALLOCATOR_WARNING",
  '"use strict";\n' + allocatorWarningSource +
    "\nreturn stripPythonAllocatorWarning;",
)(allocatorWarning);
check(
  "allocator warning is removed before JSON parsing",
  stripPythonAllocatorWarning(allocatorWarning + '\n{"argv":[]}') ===
    '{"argv":[]}',
);
check(
  "allocator warning alone becomes an empty stream",
  stripPythonAllocatorWarning(allocatorWarning) === "",
);
check(
  "unrelated stderr remains visible",
  stripPythonAllocatorWarning("real failure\n" + allocatorWarning) ===
    "real failure",
);
check(
  "similar allocator output is not hidden",
  stripPythonAllocatorWarning(allocatorWarning + " extra") ===
    allocatorWarning + " extra",
);
check(
  "registered-name check accepts the complete registry",
  missingRegisteredNames(
    [{ name: "alpha" }, { name: "beta" }, { name: "unrelated" }],
    ["alpha", "beta"],
  ).length === 0,
);
check(
  "registered-name check reports the exact absent entry",
  JSON.stringify(missingRegisteredNames([{ name: "alpha" }], ["alpha", "beta"])) ===
    JSON.stringify(["beta"]),
);
for (const required of [
  "globalThis",
  "gchatSafe",
  "orc.listScripts()",
  "verifyScriptArgPassing()",
  "orc.listUserFunctions()",
  "typeof orc[name]",
  "orc.hermitDevStatus",
]) {
  check(
    "registered-surface validation exercises " + required,
    surfaceCheckSource.includes(required),
  );
}
check(
  "surface validation startup survives failures after script registration",
  registrationSource.indexOf('PLUGIN_NAME + ".surface-check"') >
      registrationSource.indexOf("orc.registerScript(ARGV_ECHO_SCRIPT_NAME") &&
    registrationSource.indexOf('PLUGIN_NAME + ".surface-check"') <
      registrationSource.indexOf("registerHermitDevSkill("),
);
check(
  "surface validation failure sends a hard warning",
  registrationSource.includes(
    '"HARD WARNING: hermit-dev surface validation failed"',
  ) && registrationSource.includes("if (!lastSelfTest.pass)"),
);
const policyReloadSource = sourceBetween(
  "export async function policyReloadHeartbeat",
  "\n// --- registered-script invocation",
);
check(
  "policy reload does not capture the policy registration helper",
  !policyReloadSource.includes("injectHermitDevPolicies") &&
    policyReloadSource.includes('orc.hermitDevActivate("periodic")') &&
    !policyReloadSource.includes('orc[PLUGIN_NAME + ".activate"]("periodic")'),
);

// --- report ------------------------------------------------------------------
if (failures.length > 0) {
  console.error(
    "script-arg-passing: " + passed + " passed, " + failures.length + " FAILED",
  );
  for (const failure of failures) console.error("  x " + failure);
  process.exit(1);
}
console.log(
  "script-arg-passing: " + passed + " checks passed " +
    "(argv round-trip through the shipped literal, off-by-one detection, " +
    "death-gate permit/refuse brackets, policy context pass/fail brackets, " +
    "workflow-retirement reconciliation, the persisted GChat guard, " +
    "registered-surface validation, a " +
    "cache-authority mutation control, and index.ts drift guards)",
);
