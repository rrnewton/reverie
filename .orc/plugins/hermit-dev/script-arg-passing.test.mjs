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
// arguments. Two production controls prove it (`hermitSpeculativeLandWakeSent`
// bound `$1` in 76 of 76 records; `hermitOperationalTick` runs on `$1`
// continuously). What actually breaks is calling `orc.callScriptInternal` by
// hand, whose `argsJson` parameter is a JSON STRING -- pass an array and the
// dispatcher rejects it, pass a bare name and NO positionals bind.
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

let passed = 0;
const failures = [];
function check(name, cond, detail = "") {
  if (cond) { passed++; return; }
  failures.push(name + (detail ? "  -- " + detail : ""));
}

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
  // `{}` is NOT an orc placeholder. A body containing it sends those two
  // literal characters to the program -- the observed `unknown argument: {}`.
  check(
    "the probe command uses no {} placeholder",
    !text.includes("{}"),
    text,
  );
  check(
    "the probe command asks for --json",
    text.includes("--json"),
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

function compilePolicyReloadWorkflow(functionSource, inject, orcStub) {
  const javascript = functionSource
    .replace(
      "export async function policyReloadHeartbeat",
      "async function policyReloadHeartbeat",
    )
    .replace("(wf: WfContext): Promise<void>", "(wf)")
    .replace(
      "String((err as { message?: unknown })?.message || err)",
      "String((err && err.message) || err)",
    );
  return new Function(
    "POLICY_RELOAD_INTERVAL_MS",
    "injectHermitDevPolicies",
    "orc",
    javascript + "; return policyReloadHeartbeat;",
  )(7_200_000, inject, orcStub);
}

async function probePolicyReload(inject) {
  const wakeups = [];
  const sleeps = [];
  const workflowSource = sourceBetween(
    "export async function policyReloadHeartbeat",
    "\nfunction stableAlertSignature",
  );
  const workflow = compilePolicyReloadWorkflow(workflowSource, inject, {
    sendWakeup: async (targets, title, body) => {
      wakeups.push({ targets, title, body });
    },
  });
  await workflow({
    loop: async (body) => body(),
    sleep: async (milliseconds) => sleeps.push(milliseconds),
  });
  return { sleeps, wakeups };
}

const reloadPassed = await probePolicyReload(async () => ({
  contextInjected: true,
  injectionMethod: "direct-interjection",
  policyCharacters: 123,
  policyPath: "/workspace/AGENTS.md",
}));
check("periodic reload waits two hours", reloadPassed.sleeps[0] === 7_200_000);
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
    "a cache-authority mutation control, and index.ts drift guards)",
);
