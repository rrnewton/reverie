// hermit-dev — canonical dev-hermit coordinator policy plugin.
//
// ---------------------------------------------------------------------------
// POST-1.0 ORC CONTRACT — read this before adding any orc.* call.
//
// TWO CONTEXTS, TWO SURFACES. The engine evaluates this module twice, and the
// second time it is NOT the same `orc`:
//
//   * plugin load — full capability. Every registration effect is available.
//   * workflow restart — the engine re-evaluates this module from the
//     `module_path` recorded in the workflow spec, under the reduced `workflow`
//     capability preset. `orc.pluginDir()` and the stdlib `orc.registerSkill()`
//     still work there; the config-write registration effects DO NOT.
//
// Calling an absent name does not return `undefined` — the call reaches the
// host and fails the dispatch with
//
//     Error converting from js 'undefined' into type 'function':
//     orc.<name> is not a function
//
// which crashes the restarting workflow, which restarts, which re-evaluates
// this module, which crashes… until the engine gives up ("exhausted N
// restarts") and the heartbeat is silently dead. Both workflows in this plugin
// died exactly that way. The originally-reported name, `readEvalModulePath`,
// was never a published API — but it was only the FIRST such call in the file.
// Removing it just moved the crash to `orc.registerScript`.
//
// Rules that follow:
//   1. Never name an orc.* API outside `orc.listEffects()` / the shipped stdlib
//      (registerSkill, activateSkill, exposeFunction, session* live in the
//      stdlib, which is why they are absent from ~/.orc/orc.d.ts yet alive).
//   2. `typeof orc.<name> === "function"` is NOT a capability probe — it passes
//      for names the host does not publish. Use `hasOrcSurface()`, and read its
//      caveat: the registry is a PARTIAL authority (measured — it does not
//      enumerate `exposeFunction` or `registerStartup`, both of which work).
//   3. Every side effect at module scope belongs inside
//      `registerPluginSurface()`, which is invoked through the reduced-context
//      guard at the bottom of this file. Anything you add outside that function
//      runs again on every workflow restart, in the reduced context.
// ---------------------------------------------------------------------------

const PLUGIN_NAME = "hermit-dev";
const SKILL_NAME = "hermit-dev";
const SPECULATIVE_ATTACK_SKILL_NAME = "hermit-parallel-speculative-attack";
const URGENT_VALIDATION_SKILL_NAME =
  "hermit-urgent-critical-path-fix-validation";
const POLICY_CACHE_KEY = "hermit-dev.agents-policy";
const WORKSPACE_SUBPATH = "work/dev-hermit";
const PLUGIN_SUBPATH = ".orc/plugins/hermit-dev";
const POLICY_FILE = "AGENTS.md";
const SPECULATIVE_ATTACK_SKILL_FILE = "parallel-speculative-attack.md";
const URGENT_VALIDATION_SKILL_FILE = "urgent-critical-path-fix-validation.md";
const ISSUE_CREATE_WRAPPER_FILE = "gh-issue-create";

// orc.* names this plugin depends on that the runtime registry can actually
// attest, checked live at startup so a future removal reports itself instead of
// crash-looping. Deliberately NOT the full dependency list: `exposeFunction` and
// `registerStartup` are used by this plugin and work, yet neither
// orc.listEffects() nor orc.listUserFunctions() enumerates them on this build
// (measured). Listing them here produces two false positives, and a self-check
// that cries wolf is worse than none. Their removal would fail loudly at plugin
// load rather than silently, which is the failure mode this check exists for.
const REQUIRED_ORC_SURFACE = [
  "activateSkill",
  "killWorkflow",
  "kvGet",
  "kvSet",
  "listAgents",
  "log",
  "pluginDir",
  "readFile",
  "registerScript",
  "registerSkill",
  "sendWakeup",
  "userInfo",
  "workflow",
];

// Used only by this plugin, not enumerable — see REQUIRED_ORC_SURFACE.
const UNVERIFIABLE_ORC_SURFACE = ["exposeFunction", "registerStartup"];

// A dispatch against a name the current context does not publish fails with
// this phrase. It is the only way to tell the reduced workflow-restart context
// from a genuine registration bug, so it is matched deliberately and narrowly.
const ABSENT_NAME_SIGNATURE = "is not a function";

// orc.pluginDir() is null whenever this plugin is loaded by `import` from the
// project .orc/config.js instead of being registered as a plugin — which is how
// dev-hermit loads it today (the boot record shows plugins: [], plugin_count: 0).
// Everything downstream must therefore survive null; resolveSources() does the
// real work asynchronously, off the module top level.
const REGISTERED_PLUGIN_DIRECTORY = orc.pluginDir();
const PLUGIN_DIR_WORKSPACE_ROOT = REGISTERED_PLUGIN_DIRECTORY
  ? REGISTERED_PLUGIN_DIRECTORY + "/../../.."
  : null;

// Registered scripts are run by a shell, so an unexpanded $HOME is fine there.
// orc.readFile() gets no shell — file reads go through resolveSources() and its
// concrete, verified root instead.
const SHELL_WORKSPACE_ROOT = PLUGIN_DIR_WORKSPACE_ROOT ||
  ("$HOME/" + WORKSPACE_SUBPATH);

const PR_STATUS_COMMAND = 'cd "' + SHELL_WORKSPACE_ROOT +
  '" && ./ci-hub/ci-hub health';
const OPERATIONAL_TICK_SCRIPT_NAME = "hermitOperationalTick";
const OPERATIONAL_TICK_COMMAND =
  'cd "' + SHELL_WORKSPACE_ROOT + '" && ' +
  'HERMIT_AGENT_SNAPSHOT_JSON="$1" ./ci-hub/bin/health-tick --flush --no-header';
const OPERATIONAL_TICK_INTERVAL_MS = 5 * 60 * 1000;
const OPERATIONAL_TICK_WORKFLOW_NAME = "hermit-dev-operational-health-v1";
const SPECULATIVE_LAND_SCRIPT_NAME = "hermitSpeculativeLandObligations";
const SPECULATIVE_LAND_COMMAND =
  'cd "' + SHELL_WORKSPACE_ROOT + '" && ' +
  'python3 ci-hub/remediation/land_and_arm.py recover --observe-timeout 5 && ' +
  './ci-hub/ci-hub watch-obligations --once --gate';
const SPECULATIVE_LAND_WAKE_SCRIPT_NAME = "hermitSpeculativeLandWakeSent";
const SPECULATIVE_LAND_WAKE_COMMAND =
  'cd "' + SHELL_WORKSPACE_ROOT + '" && ' +
  './ci-hub/ci-hub record-obligation-wake --target "$1" --source orc';
// MANDATORY before close+respawn. The mission mandate authorizes replacing a
// broken agent with no permission, which pairs an UNVERIFIED alarm with an
// IRREVERSIBLE action. This is the check that must sit between them, and it is
// registered here for one reason: the coordinator HAS NO SHELL, so a safeguard
// it cannot invoke is not a safeguard -- that is exactly how the
// coordinator-only closure gateway became unsatisfiable. Reads /proc for
// DG_AGENT_NAME, a source independent of every gate that can raise the alarm.
// Never kills anything. rc 0 = verified dead (replace), 1 = alive (REFUSE),
// 2 = unverifiable (REFUSE).
const AGENT_LIVENESS_SCRIPT_NAME = "hermitVerifyAgentDeath";
const AGENT_LIVENESS_COMMAND =
  'cd "' + SHELL_WORKSPACE_ROOT + '" && ' +
  'python3 ci-hub/health/agent_liveness_probe.py "$1" --json';

// ARGUMENT PASSING, measured rather than assumed -- read this before "fixing"
// an empty `$1` again.
//
// `orc.registerScript` binds call arguments to SHELL POSITIONALS. There is no
// `{}` placeholder anywhere in the orc surface, so a body containing `{}` sends
// those two literal characters to the program (observed 2026-08-10 as
// `unknown argument: {}`). Use `"$1"`.
//
// The binding works, and two independent production controls prove it rather
// than one reading of the docs:
//   * `hermitSpeculativeLandWakeSent` (`--target "$1"`) wrote 76 records via
//     this surface with target `coordinator` (39) or `hermit-lander` (37) and
//     EMPTY IN ZERO of 76.
//   * `hermitOperationalTick` (`HERMIT_AGENT_SNAPSHOT_JSON="$1"`) has run on
//     that argument continuously.
// Both call through `orc.scripts.<name>(...)`.
//
// WHAT DOES BREAK, and how an empty `$1` is actually produced: calling
// `orc.callScriptInternal` by hand. Its signature is
// `callScriptInternal(name, argsJson?)` and `argsJson` is a **JSON STRING**.
// Hand it an array and the dispatcher rejects the call; hand it a bare name and
// `argsJson` is not valid JSON, so NO positionals bind and the script runs with
// an empty `$1`. Nothing was dropped -- nothing well-formed was supplied.
//
// `orc.scripts` also LOOKS broken and is not: it is a `Proxy` whose `get` trap
// manufactures a function for any name, so it has no own enumerable properties
// and inspects as `{}` while every property access is callable. Believing it
// empty is what pushes callers onto the raw internal.
//
// Prefer `callRegisteredScript()` below, which marshals correctly for both.
const ARGV_ECHO_SCRIPT_NAME = "hermitEchoScriptArgv";
// INERT BY CONSTRUCTION. It echoes its own argv as JSON and can authorize,
// mutate, and kill nothing -- the fixture discipline this repo already applies
// to the closure gateway. It reports `$0` SEPARATELY from `"$@"` so the answer
// distinguishes correct binding from the classic `sh -c` off-by-one (where the
// first argument lands in `$0` and `$1` is empty) instead of merely showing
// that something is wrong.
const ARGV_ECHO_COMMAND =
  "python3 -c 'import json,sys; " +
  'print(json.dumps({"dollar_zero": sys.argv[1], "argv": sys.argv[2:]}))\' ' +
  '"$0" "$@"';
const SPECULATIVE_LAND_INTERVAL_MS = 15 * 1000;
// Measured 2026-08-06/07: live ticks 5.1-19.3s (n=8) and 5.55-5.85s (n=5 at
// 03:03-03:05Z), out-of-band 5.1-16.8s (n=6); worst case a cold ci-hub rebuild
// at ~14s. 240s is ~12x the observed maximum. The 11-19s tail is ci-hub REBUILD
// cost, not poll cost: other agents edit ci-hub/lib/*.rs constantly and ci-hub's
// shebang is `rust-script --force`, so every call enters Cargo.
// Do NOT lower this to "tighten" it -- that converts ordinary rebuild ticks into
// false failures. Do NOT raise it either; nothing observed needs more. Exactly
// one 240s timeout exists in the whole orc log corpus, and it is still the only
// SCRIPT-effect timeout on record (the other 72 timeouts there are `sendAgent`
// effects at 120s and `summon` subprocesses at 15s -- different subsystems).
const SPECULATIVE_LAND_TIMEOUT_SEC = 240;
const SPECULATIVE_LAND_WORKFLOW_NAME =
  "hermit-dev-speculative-land-remediation-v1";
const SPECULATIVE_LAND_ALERT_CACHE_KEY =
  "hermit-dev.speculative-land-remediation-alert";
const LEGACY_PR_HEALTH_WORKFLOW_NAME = "hermit-dev-pr-health";

// Both heartbeats are load-bearing for the self-healing fleet, and cold boot
// routinely restarts them for reasons that have nothing to do with this plugin
// ("restore: timed out waiting for restored effect registration" hit ~14
// workflows in the crash-loop boot). The stock policy burned all four attempts
// inside one second and left the heartbeat permanently dead; back off instead.
const HEARTBEAT_RESTARTABLE = Object.freeze({
  maxRestarts: 100,
  backoffMs: 5_000,
  maxBackoffMs: 120_000,
});

const SKILL_DESCRIPTION = "Project-specific coordination, fork-only issue, " +
  "Git/PR, Reverie API, and product-vision policies for dev-hermit.";
const SKILL_FUNCTIONS = [
  PLUGIN_NAME + ".activate",
  PLUGIN_NAME + ".status",
  PLUGIN_NAME + ".selftest",
];
const SKILL_TRIGGERS = [
  "\\bdev-hermit\\b",
  "\\brrnewton/hermit\\b",
  "\\bfacebookexperimental/hermit\\b",
  "\\brrnewton/reverie\\b",
  "\\bfacebookexperimental/reverie\\b",
  "\\bgh\\s+issue\\s+create\\b",
  "\\bReverie\\b",
];
const SPECULATIVE_ATTACK_SKILL_DESCRIPTION =
  "Coordinator-only protocol for deadline-driven or quantified critical-path " +
  "parallel speculative attacks. Single-path execution remains the default.";
const SPECULATIVE_ATTACK_SKILL_TRIGGERS = [
  "\\bparallel[- ]speculative[- ]attack\\b",
  "\\bspeculative attack\\b",
  "\\bcompeting (?:draft )?PRs?\\b",
  "\\bdeadline[- ]driven fan[- ]out\\b",
  "\\bquantified critical[- ]path bottleneck\\b",
];
const URGENT_VALIDATION_SKILL_DESCRIPTION =
  "Coordinator-only fast validation loop for deadline-driven or quantified " +
  "critical-path fixes: parallel local and GitHub CI, active CI babysitting, " +
  "and tight local iteration on the individual failing test.";
const URGENT_VALIDATION_SKILL_TRIGGERS = [
  "\\burgent critical[- ]path (?:fix )?validation\\b",
  "\\bCI[- ]on[- ](?:the[- ])?critical[- ]path\\b",
  "\\bdeadline[- ]driven validation\\b",
  "\\bbabysit (?:the )?CI\\b",
  "\\btight local test loop\\b",
];

interface HermitDevSources {
  workspaceRoot: string;
  sourceDirectory: string;
  policyPath: string;
  policy: string;
}

let resolvedSources: HermitDevSources | null = null;
let lastSurfaceGap: string[] = [];
let lastSelfTest: SelfTestResult | null = null;

// The only safe capability probe on this build: ask the runtime what it
// publishes rather than poking a name at the host object. Returns null when
// introspection itself is unavailable, so callers can tell "absent" from
// "unknown" instead of silently treating one as the other.
function orcSurfaceNames(): Set<string> | null {
  const names = new Set<string>();
  let sawAny = false;
  const collect = (entries: unknown) => {
    if (!Array.isArray(entries)) {
      return;
    }
    sawAny = true;
    for (const entry of entries) {
      const name = entry && (entry as { name?: unknown }).name;
      if (typeof name === "string") {
        names.add(name);
      }
    }
  };
  try {
    collect(orc.listEffects({ all: true }));
  } catch (_err) {
    // Effect introspection is itself an effect; tolerate its absence.
  }
  try {
    collect(orc.listUserFunctions());
  } catch (_err) {
    // Stdlib helpers (registerSkill, exposeFunction, …) surface here.
  }
  return sawAny ? names : null;
}

function hasOrcSurface(name: string): boolean {
  const names = orcSurfaceNames();
  return names === null ? false : names.has(name);
}

// Which of `required` the running build does not publish. Empty when the
// surface is intact; also empty (not "everything missing") when introspection is
// unavailable — an unreadable registry is not evidence of removal, and failing
// closed on a probe is the bug this plugin just had. Parameterised so the
// self-test exercises this exact function rather than a lookalike.
function missingFrom(required: string[]): string[] {
  const names = orcSurfaceNames();
  if (names === null) {
    return [];
  }
  return required.filter((name) => !names.has(name));
}

function missingOrcSurface(): string[] {
  return missingFrom(REQUIRED_ORC_SURFACE);
}

// --- self-test ---------------------------------------------------------------
// The startup surface check reports "nothing missing" in two very different
// situations: the surface really is intact, or the check is dead and returning
// empty for its own reasons. An all-clear that cannot distinguish those is not
// evidence. This runs at startup, against the real registry, and brackets the
// check both ways.
//
// The probe name below is a bare string used only for a set-membership test. It
// is never called and grants nothing — planting a working capability to test a
// capability check would be the very mistake this is guarding against.
const ABSENT_PROBE_NAME = "__hermit_dev_absent_probe__";

interface SelfTestCheck {
  name: string;
  pass: boolean;
  severity: "error" | "notice";
  detail: string;
}

interface SelfTestResult {
  pass: boolean;
  checks: SelfTestCheck[];
}

function hermitDevSelfTest(): SelfTestResult {
  const checks: SelfTestCheck[] = [];
  const add = (
    name: string,
    pass: boolean,
    detail: string,
    severity: "error" | "notice" = "error",
  ) => {
    checks.push({ name, pass, severity, detail });
  };

  const names = orcSurfaceNames();
  if (names === null) {
    add(
      "registry-readable",
      false,
      "orc.listEffects()/listUserFunctions() enumerated nothing, so " +
        "missingOrcSurface() is empty because the check is blind, not because " +
        "the surface is intact",
    );
    return { pass: false, checks };
  }
  add("registry-readable", true, names.size + " names enumerated");

  // POSITIVE — the qualifying case: every required name really is enumerable,
  // so the production check is quiet for the right reason.
  const gap = missingOrcSurface();
  add(
    "required-surface-present",
    gap.length === 0,
    gap.length === 0
      ? REQUIRED_ORC_SURFACE.length + "/" + REQUIRED_ORC_SURFACE.length +
        " required names enumerated"
      : "absent: " + gap.join(", "),
  );

  // NEGATIVE — the violating case, through the SAME function: a name that is
  // genuinely not published must be reported, and it must be the ONLY thing the
  // planted run adds. Without this, the empty result above would be
  // indistinguishable from a detector that can no longer fire at all.
  //
  // Measured against the baseline gap rather than against zero, so a real
  // missing dependency reds `required-surface-present` alone instead of reding
  // this one too — one defect should light one check.
  const planted = missingFrom(REQUIRED_ORC_SURFACE.concat([ABSENT_PROBE_NAME]));
  add(
    "detector-fires-on-absent-name",
    planted.indexOf(ABSENT_PROBE_NAME) !== -1 &&
      planted.length === gap.length + 1,
    "planted " + ABSENT_PROBE_NAME + " -> reported " + JSON.stringify(planted) +
      " against a baseline gap of " + gap.length,
  );

  // The two names that work but are not enumerable must stay OUT of the
  // required list, or every startup reports two false positives — measured, and
  // exactly what happened before this list was trimmed to 13.
  const overlap = REQUIRED_ORC_SURFACE.filter((name) =>
    UNVERIFIABLE_ORC_SURFACE.indexOf(name) !== -1
  );
  add(
    "required-excludes-unverifiable",
    overlap.length === 0,
    overlap.length === 0
      ? "required list holds none of: " + UNVERIFIABLE_ORC_SURFACE.join(", ")
      : "false-positive names in the required list: " + overlap.join(", "),
  );

  // Notice, not error: if a future build starts enumerating them that is an
  // improvement, but the split above is then stale and should be collapsed.
  const nowEnumerable = UNVERIFIABLE_ORC_SURFACE.filter((name) =>
    names.has(name)
  );
  add(
    "unverifiable-still-unenumerable",
    nowEnumerable.length === 0,
    nowEnumerable.length === 0
      ? UNVERIFIABLE_ORC_SURFACE.join(", ") +
        " absent from the registry, as measured"
      : "now enumerable, move into REQUIRED_ORC_SURFACE: " +
        nowEnumerable.join(", "),
    "notice",
  );

  const pass = checks.every((check) =>
    check.pass || check.severity === "notice"
  );
  return { pass, checks };
}

function formatSelfTest(result: SelfTestResult): string {
  const failed = result.checks.filter((check) => !check.pass);
  const head = "hermit-dev self-test " + (result.pass ? "PASS" : "FAIL") +
    " (" + (result.checks.length - failed.length) + "/" +
    result.checks.length + " checks)";
  if (failed.length === 0) {
    return head;
  }
  return head + "\n" + failed
    .map((check) =>
      "  [" + check.severity + "] " + check.name + ": " + check.detail
    )
    .join("\n");
}

async function readFileIfPresent(path: string): Promise<string | null> {
  try {
    const text = String(await orc.readFile(path));
    return text.trim().length > 0 ? text : null;
  } catch (_err) {
    return null;
  }
}

// Candidate workspace roots, most authoritative first. Each is a concrete
// filesystem path — never a shell-expandable one, because orc.readFile() has no
// shell to expand it.
async function candidateWorkspaceRoots(): Promise<string[]> {
  const roots: string[] = [];
  const add = (root: string | null | undefined) => {
    if (root && roots.indexOf(root) === -1) {
      roots.push(root);
    }
  };

  add(PLUGIN_DIR_WORKSPACE_ROOT);
  try {
    const info = await orc.userInfo();
    if (info && info.homeDir) {
      add(info.homeDir + "/" + WORKSPACE_SUBPATH);
    }
  } catch (_err) {
    // userInfo may be unavailable in constrained plugin tests.
  }
  if (hasOrcSurface("repoRoot")) {
    try {
      const repoRoot = await orc.repoRoot();
      if (repoRoot) {
        add(String(repoRoot));
      }
    } catch (_err) {
      // Not every session has a repo root.
    }
  }
  return roots;
}

// A root only counts when it carries BOTH the policy file and this plugin's own
// skill sources. AGENTS.md alone is a proxy — plenty of repositories have one —
// and accepting it would let the plugin activate someone else's policy.
async function resolveSources(): Promise<HermitDevSources> {
  const roots = await candidateWorkspaceRoots();
  const tried: string[] = [];

  for (const workspaceRoot of roots) {
    const policyPath = workspaceRoot + "/" + POLICY_FILE;
    const sourceDirectory = REGISTERED_PLUGIN_DIRECTORY ||
      (workspaceRoot + "/" + PLUGIN_SUBPATH);
    tried.push(policyPath);

    const policy = await readFileIfPresent(policyPath);
    if (policy === null) {
      continue;
    }
    const skillProbe = await readFileIfPresent(
      sourceDirectory + "/" + SPECULATIVE_ATTACK_SKILL_FILE,
    );
    if (skillProbe === null) {
      continue;
    }

    resolvedSources = { workspaceRoot, sourceDirectory, policyPath, policy };
    return resolvedSources;
  }

  throw new Error(
    "hermit-dev sources not found. Every candidate must carry both " +
      POLICY_FILE + " and " + PLUGIN_SUBPATH + "/" +
      SPECULATIVE_ATTACK_SKILL_FILE + ". Tried: " +
      (tried.length > 0 ? tried.join(", ") : "(no candidate roots resolved)"),
  );
}

function registerHermitDevSkill(instructions: string): void {
  orc.registerSkill(SKILL_NAME, {
    description: SKILL_DESCRIPTION,
    instructions,
    functions: SKILL_FUNCTIONS,
    triggers: SKILL_TRIGGERS,
  });
}

function registerSpeculativeAttackSkill(instructions: string): void {
  orc.registerSkill(SPECULATIVE_ATTACK_SKILL_NAME, {
    description: SPECULATIVE_ATTACK_SKILL_DESCRIPTION,
    instructions,
    triggers: SPECULATIVE_ATTACK_SKILL_TRIGGERS,
  });
}

function registerUrgentValidationSkill(instructions: string): void {
  orc.registerSkill(URGENT_VALIDATION_SKILL_NAME, {
    description: URGENT_VALIDATION_SKILL_DESCRIPTION,
    instructions,
    triggers: URGENT_VALIDATION_SKILL_TRIGGERS,
  });
}

async function loadCoordinatorSkill(
  sources: HermitDevSources,
  fileName: string,
  register: (instructions: string) => void,
): Promise<void> {
  const path = sources.sourceDirectory + "/" + fileName;
  const instructions = await readFileIfPresent(path);
  if (instructions === null) {
    throw new Error(
      "hermit-dev coordinator skill not found or empty: " + path,
    );
  }
  register(instructions);
}

async function activateHermitDevPolicies(): Promise<string> {
  const sources = await resolveSources();

  // Re-registering replaces the placeholder or previous policy atomically.
  registerHermitDevSkill(sources.policy);
  await loadCoordinatorSkill(
    sources,
    SPECULATIVE_ATTACK_SKILL_FILE,
    registerSpeculativeAttackSkill,
  );
  await loadCoordinatorSkill(
    sources,
    URGENT_VALIDATION_SKILL_FILE,
    registerUrgentValidationSkill,
  );

  if (orc.kvGet(POLICY_CACHE_KEY) === sources.policy) {
    return "hermit-dev policies already activated from " + sources.policyPath;
  }

  const result = String(await orc.activateSkill(SKILL_NAME));
  if (!result.toLowerCase().includes("activated")) {
    throw new Error("Failed to activate hermit-dev skill: " + result);
  }

  orc.kvSet(POLICY_CACHE_KEY, sources.policy);
  return "hermit-dev policies activated from " + sources.policyPath;
}

function actionableTickLines(report: string): string[] {
  return report.split("\n").filter((line) => {
    if (line.startsWith("ACTION:") || line.startsWith("ERROR:")) {
      return true;
    }
    return line.startsWith("HEALTH:") && !line.includes(" ok ");
  });
}

export async function operationalHealthHeartbeat(wf: WfContext): Promise<void> {
  await wf.loop(async () => {
    const agents = await orc.listAgents();
    const result = await orc.scripts.hermitOperationalTick(
      JSON.stringify(agents),
    ) as {
      exitCode: number;
      stdout?: string;
      stderr?: string;
    };
    const exitCode = Number(result.exitCode);
    const stdout = String(result.stdout || "").trim();
    const stderr = String(result.stderr || "").trim();
    const report = [stdout, stderr].filter(Boolean).join("\n");
    const actionable = actionableTickLines(stdout);
    if (exitCode !== 0 || actionable.length > 0) {
      const title = exitCode === 0
        ? "HARD WARNING: operational health requires action"
        : "HARD WARNING: operational health poll failed";
      await orc.sendWakeup(
        [],
        title,
        (report || "tick-hub returned no diagnostic output") +
          "\nRun " + PR_STATUS_COMMAND + " for the full GitHub/PR report.",
      );
    }
    await wf.sleep(OPERATIONAL_TICK_INTERVAL_MS);
  });
}

function stableAlertSignature(report: string): string {
  return report.split("\n").filter((line) => !line.startsWith("COST ")).join("\n");
}

function remediationAlertSignature(report: string): string {
  const lines = report.split("\n");
  const state = lines.find((line) => line.startsWith("state=")) || "state=unknown";
  const ids = lines.find((line) => line.startsWith("ids=")) || "ids=unknown";
  return state + "\n" + ids;
}

export async function speculativeLandRemediationHeartbeat(
  wf: WfContext,
): Promise<void> {
  await wf.loop(async () => {
    // A tick may exceed its effect bound. When it did (once, 2026-08-07T01:16:45Z),
    // the throw escaped this loop body and KILLED the workflow; it survived only
    // because HEARTBEAT_RESTARTABLE caught the corpse and restarted 5s later.
    // That is survival by safety net, not reporting: nothing recorded that a tick
    // had timed out, or how long it ran. Convert the throw into a typed value the
    // existing pollFailed branch already alerts on and dedupes.
    const startedMs = Date.now();
    let result: { exitCode: number; stdout?: string; stderr?: string };
    let timedOut = false;
    try {
      result = await orc.scripts.hermitSpeculativeLandObligations() as {
        exitCode: number;
        stdout?: string;
        stderr?: string;
      };
    } catch (err) {
      // Exit code 3 is deliberate: >1 so `pollFailed` picks it up, and not 2 so
      // it can never be mistaken for `remediation-required`. Reusing that branch
      // avoids a parallel alert path that would need its own dedupe and could rot.
      timedOut = true;
      result = {
        exitCode: 3,
        stdout: "",
        stderr: "speculative-land poll did not complete: " +
          String((err as any)?.message || err),
      };
    }
    const elapsedMs = Date.now() - startedMs;
    const exitCode = Number(result.exitCode);
    const stdout = String(result.stdout || "").trim();
    const stderr = String(result.stderr || "").trim();
    const report = [stdout, stderr].filter(Boolean).join("\n");
    const outcome = timedOut ? "timeout" : (exitCode > 1 ? "error" : "ok");
    // The bound travels WITH the duration: "ran 19s" means nothing to a reader
    // who does not know whether the budget was 20s or 240s.
    const timing = "outcome=" + outcome + " elapsed_ms=" + elapsedMs +
      " bound_ms=" + (SPECULATIVE_LAND_TIMEOUT_SEC * 1000) +
      " exit_code=" + exitCode;
    // Every tick, not only failing ones: a duration series is what turns the NEXT
    // outlier into one log line instead of another investigation. Steady state is
    // ~5.6s, so an anomaly is obvious against its own history.
    //
    // WRAPPED DELIBERATELY, and this is not defensive noise. Every other orc.log
    // in this file sits inside registerPluginSurface(), i.e. the full-capability
    // PLUGIN-LOAD context. This is the first orc.log in a `wf.loop`, which the
    // engine re-evaluates under the REDUCED workflow-restart preset (see the
    // contract at the top of this file). If `log` is absent there, the call does
    // not return undefined -- it fails the dispatch and crashes the workflow,
    // which restarts, re-evaluates, and crashes again until the engine gives up.
    // That is the exact crash-loop that killed both workflows in this plugin.
    // Instrumentation must never be able to kill the thing it instruments, so a
    // missing surface degrades to a silent no-op and the tick proceeds.
    try {
      orc.log(
        timedOut ? "error" : "info",
        SPECULATIVE_LAND_SCRIPT_NAME + " " + timing,
      );
    } catch (_logErr) {
      // Intentionally swallowed: losing one telemetry line is strictly better
      // than losing the remediation heartbeat.
    }
    const remediationRequired =
      exitCode === 2 && stdout.includes("state=remediation-required");
    const pollFailed = exitCode > 1 && !remediationRequired;
    if (remediationRequired || pollFailed) {
      const signature = remediationRequired
        ? remediationAlertSignature(stdout)
        : stableAlertSignature(report);
      if (orc.kvGet(SPECULATIVE_LAND_ALERT_CACHE_KEY) !== signature) {
        let deliveryRecorded = !remediationRequired;
        const title = remediationRequired
          ? "REMEDIATION WAKE SENT (ACK PENDING): speculative land failed verification"
          : "HARD WARNING: speculative-land obligation watcher failed";
        const agents = await orc.listAgents();
        const landerAlive = agents.some((agent: any) =>
          String(agent.name || "") === "hermit-lander" &&
          !["dead", "failed", "retired", "terminated"].includes(
            String(agent.status || "").toLowerCase(),
          )
        );
        const targets = remediationRequired && landerAlive
          ? ["hermit-lander"]
          : [];
        // `timing` is appended to the BODY only, never folded into `report`.
        // `report` feeds stableAlertSignature(), which strips only "COST " lines,
        // so an elapsed_ms embedded there would make every tick a fresh signature
        // and defeat the dedupe entirely -- turning a repeating fault into an
        // alert storm. The body carries the varying evidence; the signature stays
        // keyed on the invariant part of the failure.
        await orc.sendWakeup(
          targets,
          title,
          (report || "speculative-land watcher returned no diagnostic output") +
            "\n" + timing +
            (remediationRequired
              ? "\nThis wake is advisory; the durable obligation is authoritative. " +
                "Discover and acknowledge it with ci-hub inherit-obligations, execute " +
                "the recorded fix-forward/revert action, then close it with ci-hub " +
                "resolve-obligation."
              : "\nRun " + PR_STATUS_COMMAND + " and repair the watcher now."),
        );
        if (remediationRequired) {
          const target = targets.length > 0 ? targets[0] : "coordinator";
          const delivery = await orc.scripts.hermitSpeculativeLandWakeSent(target) as {
            exitCode: number;
            stdout?: string;
            stderr?: string;
          };
          deliveryRecorded = Number(delivery.exitCode) === 0;
          if (!deliveryRecorded) {
            await orc.sendWakeup(
              [],
              "HARD WARNING: speculative-land wake was not recorded",
              [delivery.stdout, delivery.stderr].filter(Boolean).join("\n") ||
                "record-obligation-wake returned no diagnostic output",
            );
          }
        }
        if (deliveryRecorded) {
          orc.kvSet(SPECULATIVE_LAND_ALERT_CACHE_KEY, signature);
        }
      }
    } else {
      orc.kvSet(SPECULATIVE_LAND_ALERT_CACHE_KEY, "");
    }
    await wf.sleep(SPECULATIVE_LAND_INTERVAL_MS);
  });
}

// --- registered-script invocation -------------------------------------------

interface ScriptResult {
  exitCode: number;
  stdout?: string;
  stderr?: string;
}

/** Call a registered script with arguments marshalled the way the runtime
 * actually wants them. THE ONE PLACE that knows the calling convention, so a
 * caller cannot reintroduce the empty-`$1` bug by guessing at the shape. */
async function callRegisteredScript(
  name: string,
  args: unknown[],
): Promise<ScriptResult> {
  // Preferred path: the `orc.scripts` Proxy, which is what the two production
  // controls use. It JSON-encodes `args` itself.
  const scripts = orc.scripts as Record<string, unknown> | undefined;
  if (scripts) {
    const fn = scripts[name];
    if (typeof fn === "function") {
      return await (fn as (...a: unknown[]) => Promise<ScriptResult>)(...args);
    }
  }
  // Fallback for a build without the Proxy. `argsJson` is a JSON STRING; this
  // is precisely the encoding whose absence produces an empty `$1`.
  const response = await orc.callScriptInternal(name, JSON.stringify(args)) as
    | { result?: ScriptResult }
    | ScriptResult;
  const unwrapped = (response as { result?: ScriptResult }).result;
  return (unwrapped === undefined ? response : unwrapped) as ScriptResult;
}

// --- the Hard-Invariant-16 death gate ----------------------------------------

interface AgentDeathVerdict {
  agent: string;
  state: string;
  may_replace: boolean;
  rc: number;
  reason: string;
  live_pids: number[];
}

/** Every refusal shape in one place, so no branch can forget `may_replace`. */
function deathGateRefusal(agent: string, reason: string): AgentDeathVerdict {
  return {
    agent,
    state: "unverifiable",
    may_replace: false,
    rc: 2,
    reason,
    live_pids: [],
  };
}

/** MANDATORY before close+respawn (Hard Invariant 16). rc 0 verified-dead,
 * 1 alive, 2 unverifiable; REFUSE on 1 and 2 alike, because failing to prove
 * life is not proving death.
 *
 * Registered as a top-level function because the effects reference advertised
 * `orc.hermitVerifyAgentDeath(name)` while `registerScript` only ever creates
 * `orc.scripts.<name>` -- so the documented call threw `not a function`, and
 * the workaround people reached for was the raw internal that silently drops
 * arguments. A safeguard the coordinator cannot invoke is not a safeguard.
 *
 * Fail-closed at EVERY exit: a blank name, an unparseable body, a verdict about
 * the wrong subject, and any thrown error all return `may_replace: false`. */
async function hermitVerifyAgentDeath(
  agent: string,
): Promise<AgentDeathVerdict> {
  if (typeof agent !== "string" || agent.trim().length === 0) {
    return deathGateRefusal(
      typeof agent === "string" ? agent : "",
      "no agent name was supplied; refusing rather than asking the probe a " +
        "question about nobody. This is the exact 2026-08-10 fail-open, where " +
        "an empty name produced a confident verified-dead for every query.",
    );
  }

  let result: ScriptResult;
  try {
    result = await callRegisteredScript(AGENT_LIVENESS_SCRIPT_NAME, [agent]);
  } catch (err) {
    return deathGateRefusal(agent, "probe invocation failed: " + String(err));
  }

  // The probe prints its verdict to STDOUT only when it permits, and to STDERR
  // when it refuses. Reading stdout alone would see an empty body for every
  // ALIVE verdict -- a refusal that looks like no answer at all.
  const raw = String(result.stdout || "").trim() ||
    String(result.stderr || "").trim();
  if (!raw) {
    return deathGateRefusal(
      agent,
      "probe produced no output (exitCode " + String(result.exitCode) + ")",
    );
  }

  let parsed: Partial<AgentDeathVerdict>;
  try {
    parsed = JSON.parse(raw) as Partial<AgentDeathVerdict>;
  } catch (_err) {
    return deathGateRefusal(agent, "probe output was not JSON: " + raw);
  }

  // PROXY BINDING, made executable. A verdict is only about the agent it
  // NAMES. This single comparison is what the original bug needed and lacked:
  // the probe answered `{"agent": ""}` and the caller read `may_replace: true`
  // without ever noticing the answer was about a different (empty) subject.
  if (parsed.agent !== agent) {
    return deathGateRefusal(
      agent,
      "probe answered about " + JSON.stringify(parsed.agent) + " but was " +
        "asked about " + JSON.stringify(agent) +
        "; the verdict is not bound to this subject",
    );
  }

  // Permit only on unanimous agreement between the process exit code and the
  // decoded body. Anything else refuses.
  const permitted = Number(result.exitCode) === 0 &&
    parsed.state === "verified-dead" &&
    parsed.may_replace === true;

  return {
    agent,
    state: String(parsed.state || "unverifiable"),
    may_replace: permitted,
    rc: Number(result.exitCode),
    reason: String(parsed.reason || ""),
    live_pids: Array.isArray(parsed.live_pids) ? parsed.live_pids : [],
  };
}

// --- script discovery ---------------------------------------------------------

interface DiscoveredScript {
  name: string;
  description: string | null;
  call: string;
  takesArguments: boolean;
}

/** Enumerate the registered scripts WITH the syntax that actually calls them.
 *
 * `orc.scripts` is a `Proxy` whose `get` trap manufactures a function on
 * demand, so it has no own enumerable properties: `Object.keys()` is `[]` and
 * it inspects as `{}`. That is indistinguishable from "no scripts registered",
 * and believing it empty is what sends people to the raw
 * `orc.callScriptInternal`, whose `argsJson` parameter they then get wrong --
 * which is how a script ends up running with an empty `$1`.
 *
 * The registry itself IS enumerable via `orc.listScripts()`. This joins the two
 * so the answer to "what can I call, and how?" is one call and needs no
 * knowledge of the Proxy. */
async function hermitDevListScripts(): Promise<{
  scripts: DiscoveredScript[];
  note: string;
}> {
  let listed: unknown[] = [];
  try {
    listed = await orc.listScripts() as unknown[];
  } catch (err) {
    return {
      scripts: [],
      note: "orc.listScripts() failed: " + String(err) +
        " -- this is a registry read failure, NOT evidence that no scripts " +
        "are registered.",
    };
  }

  const scripts = (Array.isArray(listed) ? listed : []).map((entry) => {
    const row = (entry || {}) as { name?: string; description?: string; script?: string };
    const name = String(row.name || "");
    // A body referencing a positional expects arguments; one that does not,
    // takes none. Reported so a caller does not have to read the shell.
    const body = String(row.script || "");
    const takesArguments = /\$\{?[1-9]/.test(body) || body.includes('"$@"');
    return {
      name,
      description: row.description === undefined ? null : String(row.description),
      call: "await orc.scripts." + name + "(" +
        (takesArguments ? "arg1, …" : "") + ")",
      takesArguments,
    };
  });

  return {
    scripts,
    note: "orc.scripts is a Proxy: it inspects as {} and Object.keys() is " +
      "empty, but every name above is callable on it. Arguments bind to SHELL " +
      "POSITIONALS ($1, $2, …). There is NO {} placeholder -- a body " +
      "containing {} sends those two literal characters to the program. Do " +
      "not hand-roll orc.callScriptInternal: its argsJson parameter is a JSON " +
      "STRING, and getting that wrong binds no positionals at all.",
  };
}

// --- argv bracket ------------------------------------------------------------
// The claim "the surface drops every argument" and the claim "my call shape was
// wrong" predict DIFFERENT observations, and only a run can separate them. This
// makes that run one call, against the live registry, with the counts stated.

async function verifyScriptArgPassing(): Promise<SelfTestResult> {
  const checks: SelfTestCheck[] = [];
  const add = (
    name: string,
    pass: boolean,
    detail: string,
    severity: "error" | "notice" = "error",
  ) => {
    checks.push({ name, pass, severity, detail });
  };

  // POSITIVE: distinctive multi-argument payload, including a space and an
  // empty string, so a truncating or re-splitting binder cannot coincidentally
  // look correct.
  const sent = ["alpha", "two words", "", "4"];
  let echoed: { dollar_zero?: string; argv?: string[] } | null = null;
  let rawBody = "";
  try {
    const result = await callRegisteredScript(ARGV_ECHO_SCRIPT_NAME, sent);
    rawBody = String(result.stdout || "").trim() ||
      String(result.stderr || "").trim();
    echoed = JSON.parse(rawBody) as { dollar_zero?: string; argv?: string[] };
  } catch (err) {
    add("argv-echo-callable", false, "echo script failed: " + String(err));
    return { pass: false, checks };
  }
  add("argv-echo-callable", true, "returned " + rawBody);

  const got = Array.isArray(echoed.argv) ? echoed.argv : [];
  const exact = got.length === sent.length &&
    sent.every((value, index) => got[index] === value);
  add(
    "arguments-arrive-exactly",
    exact,
    "sent " + JSON.stringify(sent) + " -> received " + JSON.stringify(got) +
      " (" + got.length + "/" + sent.length + " positionals)",
  );

  // The off-by-one this bug was blamed on: with `sh -c cmd arg1`, `$0` becomes
  // arg1 and `$1` is empty. Naming it explicitly means a future failure reports
  // WHICH binding broke rather than just "empty".
  add(
    "no-sh-c-off-by-one",
    echoed.dollar_zero !== sent[0],
    "$0=" + JSON.stringify(echoed.dollar_zero) + "; first argument sent was " +
      JSON.stringify(sent[0]) +
      (echoed.dollar_zero === sent[0]
        ? " -- ARGUMENTS ARE SHIFTED: $1 is empty because arg 1 landed in $0"
        : " -- distinct, so positionals are not shifted"),
  );

  // NEGATIVE: the same path with no arguments must come back genuinely empty.
  // Without this, "arguments arrive" could be satisfied by a stub echoing a
  // canned payload regardless of input.
  try {
    const bare = await callRegisteredScript(ARGV_ECHO_SCRIPT_NAME, []);
    const body = String(bare.stdout || "").trim();
    const parsed = JSON.parse(body) as { argv?: string[] };
    const empty = Array.isArray(parsed.argv) && parsed.argv.length === 0;
    add(
      "no-arguments-yields-empty-argv",
      empty,
      "sent [] -> received " + JSON.stringify(parsed.argv) +
        " (echo is input-sensitive, not a canned reply)",
    );
  } catch (err) {
    add("no-arguments-yields-empty-argv", false, String(err));
  }

  // The death gate itself, end to end, refusing on a blank subject. Inert: it
  // reads /proc and returns a verdict, and nothing here acts on one.
  try {
    const blank = await hermitVerifyAgentDeath("");
    add(
      "death-gate-refuses-blank-subject",
      blank.may_replace === false && blank.rc === 2,
      "blank name -> rc " + blank.rc + " may_replace " + blank.may_replace,
    );
  } catch (err) {
    add("death-gate-refuses-blank-subject", false, String(err));
  }

  const pass = checks.every((check) =>
    check.pass || check.severity === "notice"
  );
  return { pass, checks };
}

// --- registration ----------------------------------------------------------
// Every side effect this module has lives in here, and it is reached only
// through the reduced-context guard at the bottom of the file. Nothing above
// this point touches the host except the one `orc.pluginDir()` that the
// workflow-restart context is known to serve.

function registerPluginSurface(): void {
  // ORDER IS LOAD-BEARING — orc.registerScript goes FIRST because it is the
  // effect the reduced workflow-restart context does not publish, so the guard
  // below aborts this function here rather than one step later. The step after
  // it re-registers PLACEHOLDER skill text, and the reduced context DOES serve
  // orc.registerSkill (measured: three registerSkill calls succeeded there).
  // Letting a restart reach that step would quietly replace the activated
  // AGENTS.md policy with the "loaded during startup" stub and leave the
  // coordinator running on a placeholder until the next activation — a silent
  // policy downgrade, worse than the crash it replaced. Do not reorder.
  orc.registerScript(OPERATIONAL_TICK_SCRIPT_NAME, {
    script: OPERATIONAL_TICK_COMMAND,
    description: "Run the version-pinned dev-hermit tick-hub operational poll",
    timeoutSec: 180,
  });

  orc.registerScript(SPECULATIVE_LAND_SCRIPT_NAME, {
    script: SPECULATIVE_LAND_COMMAND,
    description: "Poll exact-SHA speculative-land obligations for immediate remediation",
    timeoutSec: SPECULATIVE_LAND_TIMEOUT_SEC,
  });

  orc.registerScript(AGENT_LIVENESS_SCRIPT_NAME, {
    script: AGENT_LIVENESS_COMMAND,
    description:
      "REQUIRED before close+respawn: independently verify a named agent is " +
      "genuinely dead (rc 0 dead / 1 alive / 2 unverifiable; refuse on 1 or 2)",
    timeoutSec: 60,
  });

  orc.registerScript(SPECULATIVE_LAND_WAKE_SCRIPT_NAME, {
    script: SPECULATIVE_LAND_WAKE_COMMAND,
    description: "Record an ORC wake as sent but not yet acknowledged",
    timeoutSec: 30,
  });

  orc.registerScript(ARGV_ECHO_SCRIPT_NAME, {
    script: ARGV_ECHO_COMMAND,
    description:
      "Diagnostic: echo this script's own argv as JSON (inert; authorizes and " +
      "changes nothing). Answers 'did my arguments arrive?' directly",
    timeoutSec: 30,
  });

  // Placeholders only; startup replaces all three with the real policy text.
  registerHermitDevSkill(
    "The canonical dev-hermit policies are loaded from AGENTS.md during startup.",
  );
  registerSpeculativeAttackSkill(
    "The parallel speculative attack protocol is loaded during plugin startup.",
  );
  registerUrgentValidationSkill(
    "The urgent critical-path validation protocol is loaded during plugin " +
      "startup.",
  );

  orc.exposeFunction(
    PLUGIN_NAME + ".activate",
    activateHermitDevPolicies,
    {
      description: "Reload AGENTS.md and activate the canonical dev-hermit policies",
      params: [],
      sig: "await orc.hermit-dev.activate()",
    },
  );

  // Top-level, NOT plugin-namespaced: this is the exact name Hard Invariant 16
  // and the effects reference already tell the coordinator to call.
  orc.exposeFunction(
    AGENT_LIVENESS_SCRIPT_NAME,
    hermitVerifyAgentDeath,
    {
      description:
        "REQUIRED before close+respawn: independently verify a named agent is " +
        "genuinely dead. Returns {agent, state, may_replace, rc, reason, " +
        "live_pids}. Replace ONLY on may_replace true; rc 1 (alive) and rc 2 " +
        "(unverifiable) both refuse. Never kills anything",
      params: [{ name: "agent", type: "string", description: "the DG_AGENT_NAME to verify" }],
      sig: "await orc.hermitVerifyAgentDeath(agent: string)",
    },
  );

  // Discoverability for a Proxy that reads as empty. Without this, "there are
  // no scripts" and "I cannot enumerate the scripts" look identical.
  orc.exposeFunction(
    PLUGIN_NAME + ".listScripts",
    hermitDevListScripts,
    {
      description:
        "List registered scripts with the exact syntax that calls them " +
        "(orc.scripts is a Proxy and inspects as {} even when populated)",
      params: [],
      sig: "await orc.hermit-dev.listScripts()",
    },
  );

  // The argv bracket, exposed separately because it is async and the sync
  // `selftest` cannot await a script. Run it whenever an argument "looks
  // dropped" -- it answers with evidence instead of inference.
  orc.exposeFunction(
    PLUGIN_NAME + ".verifyScriptArgs",
    verifyScriptArgPassing,
    {
      description:
        "Bracket registered-script argument passing both ways against the live " +
        "registry using an inert argv-echo script",
      params: [],
      sig: "await orc.hermit-dev.verifyScriptArgs()",
    },
  );

  orc.exposeFunction(
    PLUGIN_NAME + ".selftest",
    hermitDevSelfTest,
    {
      description:
        "Re-run the orc.* surface self-check (brackets it both ways) against the live registry",
      params: [],
      sig: "orc.hermit-dev.selftest()",
    },
  );

  orc.exposeFunction(
    PLUGIN_NAME + ".status",
    function hermitDevStatus() {
      const cachedPolicy = orc.kvGet(POLICY_CACHE_KEY);
      const sources = resolvedSources;
      const sourceDirectory = sources
        ? sources.sourceDirectory
        : (REGISTERED_PLUGIN_DIRECTORY ||
          SHELL_WORKSPACE_ROOT + "/" + PLUGIN_SUBPATH);
      return {
        plugin: PLUGIN_NAME,
        skill: SKILL_NAME,
        coordinatorSkills: [
          SPECULATIVE_ATTACK_SKILL_NAME,
          URGENT_VALIDATION_SKILL_NAME,
        ],
        sourcesResolved: sources !== null,
        policyPath: sources ? sources.policyPath : null,
        speculativeAttackSkillPath: sourceDirectory + "/" +
          SPECULATIVE_ATTACK_SKILL_FILE,
        urgentValidationSkillPath: sourceDirectory + "/" +
          URGENT_VALIDATION_SKILL_FILE,
        policyLoaded: typeof cachedPolicy === "string",
        policyBytes: typeof cachedPolicy === "string" ? cachedPolicy.length : 0,
        workspace: sources ? sources.workspaceRoot : SHELL_WORKSPACE_ROOT,
        registeredPluginDirectory: REGISTERED_PLUGIN_DIRECTORY,
        requiredOrcSurface: REQUIRED_ORC_SURFACE,
        missingOrcSurface: missingOrcSurface(),
        missingOrcSurfaceAtStartup: lastSurfaceGap,
        unverifiableOrcSurface: UNVERIFIABLE_ORC_SURFACE,
        selfTestAtStartup: lastSelfTest,
        hermitPrimary: "rrnewton/hermit",
        hermitUpstream: "facebookexperimental/hermit",
        reverieIssueRepo: "rrnewton/reverie",
        issueCreateWrapper: sourceDirectory + "/" + ISSUE_CREATE_WRAPPER_FILE,
        prStatusCommand: PR_STATUS_COMMAND,
        operationalTickCommand: OPERATIONAL_TICK_COMMAND,
        operationalTickConfig: SHELL_WORKSPACE_ROOT +
          "/ci-hub/health/tick-hub.yaml",
        operationalTickIntervalMinutes: OPERATIONAL_TICK_INTERVAL_MS / 60000,
        speculativeLandCommand: SPECULATIVE_LAND_COMMAND,
        speculativeLandWakeCommand: SPECULATIVE_LAND_WAKE_COMMAND,
        agentLivenessCommand: AGENT_LIVENESS_COMMAND,
        speculativeLandPollIntervalSeconds: SPECULATIVE_LAND_INTERVAL_MS / 1000,
        heartbeatRestartPolicy: HEARTBEAT_RESTARTABLE,
        maxParkedSlots: 5,
        maxActiveWorktrees: 12,
        maxAgents: 15,
      };
    },
    {
      description: "Report hermit-dev plugin registration and policy source state",
      params: [],
      sig: "orc.hermit-dev.status()",
    },
  );

  orc.workflow(
    operationalHealthHeartbeat,
    "Run tick-hub operational checks and hard-warn the coordinator on failures",
    {
      name: OPERATIONAL_TICK_WORKFLOW_NAME,
      restartable: HEARTBEAT_RESTARTABLE,
    },
  );

  orc.workflow(
    speculativeLandRemediationHeartbeat,
    "Hard-warn immediately when an exact-SHA speculative-land verifier fails",
    {
      name: SPECULATIVE_LAND_WORKFLOW_NAME,
      restartable: HEARTBEAT_RESTARTABLE,
    },
  );

  orc.registerStartup(PLUGIN_NAME + ".startup", async function hermitDevStartup() {
    // Report; never throw. A named-but-absent API is what crash-looped this
    // plugin, and a startup hook that dies takes the policies down with it.
    lastSelfTest = hermitDevSelfTest();
    orc.log(lastSelfTest.pass ? "info" : "error", formatSelfTest(lastSelfTest));

    lastSurfaceGap = missingOrcSurface();
    if (lastSurfaceGap.length > 0) {
      orc.log(
        "error",
        "hermit-dev: the running ORC build no longer publishes " +
          lastSurfaceGap.length + " required API(s): " +
          lastSurfaceGap.join(", ") +
          ". Update .orc/plugins/hermit-dev/index.ts against orc.listEffects().",
      );
    }
    if (hasOrcSurface("killWorkflow")) {
      try {
        await orc.killWorkflow(LEGACY_PR_HEALTH_WORKFLOW_NAME);
      } catch (_err) {
        // The legacy workflow is absent in new sessions.
      }
    }
    const result = await activateHermitDevPolicies();
    orc.log("info", result);
  });
}

// --- module entry ------------------------------------------------------------
// Plugin load reaches this with the full capability surface and registers
// everything. The workflow-restart path reaches it with the reduced `workflow`
// preset, where the first registration effect it meets does not exist; that is
// expected and must not propagate, because the restart is only re-evaluating
// this module to resolve the exported heartbeat function — which is already
// defined, above, with no registration required.
//
// The catch is deliberately narrow. Only the host's absent-name dispatch failure
// is swallowed; anything else is a genuine registration bug and is re-thrown so
// plugin load fails loudly instead of coming up half-registered.
try {
  registerPluginSurface();
} catch (err) {
  const message = String((err && (err as { message?: unknown }).message) || err);
  if (message.indexOf(ABSENT_NAME_SIGNATURE) === -1) {
    throw err;
  }
}
