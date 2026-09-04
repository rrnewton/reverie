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
const POLICY_RELOAD_INTERVAL_MS = 2 * 60 * 60 * 1000;
const POLICY_RELOAD_WORKFLOW_NAME = "hermit-dev-policy-reload-2h-v1";
const LEGACY_POLICY_RELOAD_WORKFLOW_NAME = "policy-reload-2h";
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
  "interjection",
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
  'HERMIT_AGENT_SNAPSHOT_JSON="$1" ' +
  './scripts/hermit-health-tick.sh --emit-output';
const OPERATIONAL_TICK_INTERVAL_MS = 30 * 60 * 1000;
const OPERATIONAL_TICK_WORKFLOW_NAME = "hermit-dev-operational-health-v1";
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
  '/usr/bin/python3 ci-hub/health/agent_liveness_probe.py "$1" --json';

// ARGUMENT PASSING, measured rather than assumed -- read this before "fixing"
// an empty `$1` again.
//
// `orc.registerScript` binds call arguments to SHELL POSITIONALS and also
// substitutes brace-delimited text before invoking the shell. The Python dict
// literal in ARGV_ECHO_COMMAND was replaced by the first argument (measured
// 2026-08-27). Avoid braces in script bodies; use `"$1"` for arguments.
//
// The binding is measured rather than inferred:
// `hermitOperationalTick` (`HERMIT_AGENT_SNAPSHOT_JSON="$1"`) runs through
// `orc.scripts.<name>(...)` continuously, and `verifyScriptArgPassing()` below
// brackets the same surface with a distinctive multi-argument payload.
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
  "/usr/bin/python3 -c 'import json,sys; " +
  'print(json.dumps(dict(dollar_zero=sys.argv[1], argv=sys.argv[2:])))\' ' +
  '"$0" "$@"';
const LEGACY_SPECULATIVE_LAND_WORKFLOW_NAME =
  "hermit-dev-speculative-land-remediation-v1";
const LEGACY_PR_HEALTH_WORKFLOW_NAME = "hermit-dev-pr-health";

// The recurring workflows are load-bearing for the self-healing fleet, and
// cold boot routinely restarts them for reasons that have nothing to do with this plugin
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
  "hermitDevActivate",
  "hermitDevStatus",
  "hermitDevSelfTest",
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

type PolicyInjectionReason = "startup" | "manual" | "periodic";

interface PolicyInjectionReceipt {
  contextInjected: true;
  injectionMethod: "activate-skill" | "direct-interjection";
  activationResult: string;
  policyPath: string;
  policyCharacters: number;
  policyWasCached: boolean;
  reason: PolicyInjectionReason;
  injectedAt: string;
}

let resolvedSources: HermitDevSources | null = null;
let lastSurfaceGap: string[] = [];
let lastSelfTest: SelfTestResult | null = null;
let lastPolicyInjection: PolicyInjectionReceipt | null = null;

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

async function injectHermitDevPolicies(
  reason: PolicyInjectionReason,
): Promise<PolicyInjectionReceipt> {
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

  const policyWasCached = orc.kvGet(POLICY_CACHE_KEY) === sources.policy;
  const result = String(await orc.activateSkill(SKILL_NAME));
  const normalizedResult = result.toLowerCase();
  let injectionMethod: PolicyInjectionReceipt["injectionMethod"];
  if (normalizedResult.includes("already activated")) {
    // activateSkill deliberately de-duplicates within one JS runtime. A reload
    // request is different: its purpose is to put the policy back into the
    // coordinator's current context after compaction or a long-running turn.
    // Direct interjection is the load-bearing operation here; do not report a
    // reload unless it resolves successfully.
    await orc.interjection(sources.policy);
    injectionMethod = "direct-interjection";
  } else if (normalizedResult.includes("activated")) {
    // stdlib activateSkill injects the registered instructions before it
    // returns this result. This is the normal cold-start/resume path.
    injectionMethod = "activate-skill";
  } else {
    throw new Error("Failed to activate hermit-dev skill: " + result);
  }

  orc.kvSet(POLICY_CACHE_KEY, sources.policy);
  const receipt: PolicyInjectionReceipt = {
    contextInjected: true,
    injectionMethod,
    activationResult: result,
    policyPath: sources.policyPath,
    policyCharacters: sources.policy.length,
    policyWasCached,
    reason,
    injectedAt: new Date().toISOString(),
  };
  lastPolicyInjection = receipt;
  return receipt;
}

async function activateHermitDevPolicies(
  reason: PolicyInjectionReason = "manual",
): Promise<string> {
  return JSON.stringify(await injectHermitDevPolicies(reason));
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

export async function policyReloadHeartbeat(wf: WfContext): Promise<void> {
  await wf.loop(async () => {
    await wf.sleep(POLICY_RELOAD_INTERVAL_MS);
    try {
      const parsedReceipt = JSON.parse(
        String(await orc.hermitDevActivate("periodic")),
      );
      if (!parsedReceipt || parsedReceipt.contextInjected !== true) {
        throw new Error("policy reload did not confirm context injection");
      }
      const receipt = parsedReceipt as PolicyInjectionReceipt;
      await orc.sendWakeup(
        [],
        "POLICY RELOAD COMPLETE",
        "context_injected=true method=" + receipt.injectionMethod +
          " policy_characters=" + receipt.policyCharacters +
          " policy_path=" + receipt.policyPath,
      );
    } catch (err) {
      await orc.sendWakeup(
        [],
        "POLICY RELOAD FAILED",
        "context_injected=false error=" +
          String((err as { message?: unknown })?.message || err),
      );
    }
  });
}

// --- registered-script invocation -------------------------------------------

interface ScriptResult {
  exitCode: number;
  stdout?: string;
  stderr?: string;
}

const PYTHON_ALLOCATOR_WARNING =
  "<jemalloc>: Invalid conf pair: experimental_infallible_new:true";

// Some ORC script paths merge a child's stderr into the value returned as
// stdout. Remove only the allocator's exact startup line before a caller parses
// JSON; every other diagnostic remains visible and therefore fail-closed.
function stripPythonAllocatorWarning(text: unknown): string {
  return String(text || "").split("\n")
    .filter((line) => line !== PYTHON_ALLOCATOR_WARNING)
    .join("\n");
}

function cleanScriptResult(result: ScriptResult): ScriptResult {
  return {
    ...result,
    stdout: stripPythonAllocatorWarning(result.stdout),
    stderr: stripPythonAllocatorWarning(result.stderr),
  };
}

/** Call a registered script with arguments marshalled the way the runtime
 * actually wants them. THE ONE PLACE that knows the calling convention, so a
 * caller cannot reintroduce the empty-`$1` bug by guessing at the shape. */
async function callRegisteredScript(
  name: string,
  args: unknown[],
): Promise<ScriptResult> {
  // Preferred path: the `orc.scripts` Proxy, which production callers use. It
  // JSON-encodes `args` itself.
  const scripts = orc.scripts as Record<string, unknown> | undefined;
  if (scripts) {
    const fn = scripts[name];
    if (typeof fn === "function") {
      const result = await (fn as (...a: unknown[]) => Promise<ScriptResult>)(
        ...args,
      );
      return cleanScriptResult(result);
    }
  }
  // Fallback for a build without the Proxy. `argsJson` is a JSON STRING; this
  // is precisely the encoding whose absence produces an empty `$1`.
  const response = await orc.callScriptInternal(name, JSON.stringify(args)) as
    | { result?: ScriptResult }
    | ScriptResult;
  const unwrapped = (response as { result?: ScriptResult }).result;
  return cleanScriptResult(
    (unwrapped === undefined ? response : unwrapped) as ScriptResult,
  );
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
      "POSITIONALS ($1, $2, …). ORC also substitutes brace-delimited text " +
      "before invoking the shell, so registered commands must avoid braces. Do " +
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

function missingRegisteredNames(
  entries: unknown,
  required: string[],
): string[] {
  const names = new Set(
    (Array.isArray(entries) ? entries : []).map((entry) => {
      const row = (entry || {}) as { name?: unknown };
      return String(row.name || "");
    }),
  );
  return required.filter((name) => !names.has(name));
}

async function verifyRegisteredSurface(): Promise<SelfTestResult> {
  const host = hermitDevSelfTest();
  const checks = host.checks.slice();
  const add = (
    name: string,
    pass: boolean,
    detail: string,
    severity: "error" | "notice" = "error",
  ) => {
    checks.push({ name, pass, severity, detail });
  };

  const guard = (globalThis as Record<string, unknown>).gchatSafe;
  if (typeof guard !== "function") {
    add("gchat-safe-callable", false, "globalThis.gchatSafe is not a function");
  } else {
    try {
      const call = guard as (text: string) => {
        clean?: boolean;
        offenders?: unknown[];
      };
      const allowed = call("PR and CI are allowed project acronyms.");
      const refused = call("THIS MESSAGE MUST BE REFUSED.");
      add(
        "gchat-safe-callable",
        allowed.clean === true && refused.clean === false &&
          Array.isArray(refused.offenders) && refused.offenders.length > 0,
        "allowed clean=" + String(allowed.clean) +
          "; caps clean=" + String(refused.clean) +
          "; offenders=" + JSON.stringify(refused.offenders),
      );
    } catch (err) {
      add("gchat-safe-callable", false, "call failed: " + String(err));
    }
  }

  let listedScripts: unknown = null;
  try {
    listedScripts = await orc.listScripts();
    add(
      "script-registry-readable",
      Array.isArray(listedScripts),
      Array.isArray(listedScripts)
        ? listedScripts.length + " scripts enumerated"
        : "orc.listScripts() returned " + typeof listedScripts,
    );
  } catch (err) {
    add("script-registry-readable", false, "orc.listScripts() failed: " + String(err));
  }
  const requiredScripts = [
    OPERATIONAL_TICK_SCRIPT_NAME,
    AGENT_LIVENESS_SCRIPT_NAME,
    ARGV_ECHO_SCRIPT_NAME,
  ];
  const missingScripts = missingRegisteredNames(listedScripts, requiredScripts);
  add(
    "registered-scripts-present",
    missingScripts.length === 0,
    missingScripts.length === 0
      ? requiredScripts.length + "/" + requiredScripts.length +
        " registered scripts enumerated under orc.scripts"
      : "absent from orc.listScripts(): " + missingScripts.join(", "),
  );

  const scriptChecks = await verifyScriptArgPassing();
  checks.push(...scriptChecks.checks);

  let listedFunctions: unknown = null;
  try {
    listedFunctions = orc.listUserFunctions();
    add(
      "function-registry-readable",
      Array.isArray(listedFunctions),
      Array.isArray(listedFunctions)
        ? listedFunctions.length + " user functions enumerated"
        : "orc.listUserFunctions() returned " + typeof listedFunctions,
    );
  } catch (err) {
    add(
      "function-registry-readable",
      false,
      "orc.listUserFunctions() failed: " + String(err),
    );
  }
  const requiredFunctions = [
    "hermitDevActivate",
    "hermitDevStatus",
    "hermitDevSelfTest",
    AGENT_LIVENESS_SCRIPT_NAME,
    PLUGIN_NAME + ".activate",
    PLUGIN_NAME + ".listScripts",
    PLUGIN_NAME + ".verifyScriptArgs",
    PLUGIN_NAME + ".selftest",
    PLUGIN_NAME + ".status",
  ];
  const missingFunctions = missingRegisteredNames(
    listedFunctions,
    requiredFunctions,
  );
  add(
    "registered-functions-present",
    missingFunctions.length === 0,
    missingFunctions.length === 0
      ? requiredFunctions.length + "/" + requiredFunctions.length +
        " exposed functions enumerated"
      : "absent from orc.listUserFunctions(): " + missingFunctions.join(", "),
  );

  const unresolvedFunctions = requiredFunctions.filter((name) =>
    typeof orc[name] !== "function"
  );
  add(
    "registered-functions-resolve",
    unresolvedFunctions.length === 0,
    unresolvedFunctions.length === 0
      ? requiredFunctions.length + "/" + requiredFunctions.length +
        " exposed functions resolve through orc"
      : "not callable through orc: " + unresolvedFunctions.join(", "),
  );

  const statusFunction = orc.hermitDevStatus;
  if (typeof statusFunction !== "function") {
    add("advertised-status-callable", false, "orc.hermitDevStatus is not a function");
  } else {
    try {
      const status = await statusFunction() as { plugin?: unknown };
      add(
        "advertised-status-callable",
        status?.plugin === PLUGIN_NAME,
        "returned plugin=" + JSON.stringify(status?.plugin),
      );
    } catch (err) {
      add("advertised-status-callable", false, "call failed: " + String(err));
    }
  }

  const pass = checks.every((check) =>
    check.pass || check.severity === "notice"
  );
  return { pass, checks };
}

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
    lastPolicyContextInjection: lastPolicyInjection,
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
    policyReloadWorkflow: POLICY_RELOAD_WORKFLOW_NAME,
    policyReloadIntervalMinutes: POLICY_RELOAD_INTERVAL_MS / 60000,
    agentLivenessCommand: AGENT_LIVENESS_COMMAND,
    heartbeatRestartPolicy: HEARTBEAT_RESTARTABLE,
    maxParkedSlots: 5,
    maxActiveWorktrees: 12,
    maxAgents: 15,
  };
}

// --- registration ----------------------------------------------------------
// Every side effect this module has lives in here, and it is reached only
// through the reduced-context guard at the bottom of the file. Nothing above
// this point touches the host except the one `orc.pluginDir()` that the
// workflow-restart context is known to serve.

function registerPluginSurface(): void {
  // GCHAT SAFE START. This guard was created in one coordinator turn with a
  // globalThis assignment, so a config reload discarded it. Install the same
  // guard before any ORC registration effect: if a later registration throws,
  // messages can still be checked against the owner's no-caps rule. Keep this
  // block plain JavaScript so the regression test can execute the shipped body.
  Object.defineProperty(globalThis, "gchatSafe", {
    configurable: true,
    writable: true,
    value: function gchatSafe(text) {
      const offenders =
        (text.match(/\b[A-Z][A-Z0-9]{2,}(?:[ -][A-Z0-9]{2,})*\b/g) || [])
          .filter(function (word) {
            return !/^(PR|CI|DBT|KVM|QEMU|DAP|MCP|API|CPU|GB|GIB|MB|TSC|ABI|URL|JSON|SHA|ORC|EOF|README)$/.test(word);
          });
      return { clean: offenders.length === 0, offenders };
    },
  });
  // GCHAT SAFE END.

  // The skill renderer treats every function name as one direct orc property.
  // A dotted name containing this plugin's hyphen is therefore a flat registry
  // key, not the nested `orc["hermit-dev"].status` object the old policy text
  // advertised. Publish valid identifier aliases before any script registration
  // can fail, while retaining the old flat keys below for compatibility.
  orc.exposeFunction("hermitDevActivate", activateHermitDevPolicies, {
    description: "Reload AGENTS.md and activate the canonical dev-hermit policies",
    params: [],
    sig: "await orc.hermitDevActivate()",
  });
  orc.exposeFunction("hermitDevSelfTest", verifyRegisteredSurface, {
    description:
      "Exercise the registered hermit-dev functions and scripts against the live registries",
    params: [],
    sig: "await orc.hermitDevSelfTest()",
  });
  orc.exposeFunction("hermitDevStatus", hermitDevStatus, {
    description: "Report hermit-dev plugin registration and policy source state",
    params: [],
    sig: "orc.hermitDevStatus()",
  });

  // The policy reload workflow calls this existing exposed function instead of
  // capturing injectHermitDevPolicies and its helper graph in durable workflow
  // state. Register it before the reduced-context guard so module replay can
  // resolve the call. This does not register placeholder policy text.
  orc.exposeFunction(
    PLUGIN_NAME + ".activate",
    activateHermitDevPolicies,
    {
      description: "Reload AGENTS.md and activate the canonical dev-hermit policies",
      params: [],
      sig: 'await orc["hermit-dev.activate"]()',
    },
  );

  // ORDER IS LOAD-BEARING — orc.registerScript is the first effect that the
  // reduced workflow-restart context does not publish, so the guard below
  // aborts this function here rather than one step later. The step after the
  // scripts re-registers PLACEHOLDER skill text, and the reduced context DOES
  // serve orc.registerSkill (measured: three registerSkill calls succeeded
  // there). Letting a restart reach that step would quietly replace the
  // activated AGENTS.md policy with the "loaded during startup" stub and leave
  // the coordinator running on a placeholder until the next activation — a
  // silent policy downgrade, worse than the crash it replaced. Do not reorder.
  orc.registerScript(OPERATIONAL_TICK_SCRIPT_NAME, {
    script: OPERATIONAL_TICK_COMMAND,
    description: "Run the version-pinned dev-hermit tick-hub operational poll",
    timeoutSec: 180,
  });

  orc.registerScript(AGENT_LIVENESS_SCRIPT_NAME, {
    script: AGENT_LIVENESS_COMMAND,
    description:
      "REQUIRED before close+respawn: independently verify a named agent is " +
      "genuinely dead (rc 0 dead / 1 alive / 2 unverifiable; refuse on 1 or 2)",
    timeoutSec: 60,
  });

  orc.registerScript(ARGV_ECHO_SCRIPT_NAME, {
    script: ARGV_ECHO_COMMAND,
    description:
      "Diagnostic: echo this script's own argv as JSON (inert; authorizes and " +
      "changes nothing). Answers 'did my arguments arrive?' directly",
    timeoutSec: 30,
  });

  // Register this immediately after the scripts. Workflow replay stops at the
  // first registerScript above, while a full config load reaches this hook. If
  // a later skill, function, or workflow registration fails, the hook survives
  // and reports the exact missing registry entries instead of a host-only pass.
  orc.registerStartup(
    PLUGIN_NAME + ".surface-check",
    async function hermitDevSurfaceCheck() {
      try {
        lastSelfTest = await verifyRegisteredSurface();
        orc.log(
          lastSelfTest.pass ? "info" : "error",
          formatSelfTest(lastSelfTest),
        );
        if (!lastSelfTest.pass) {
          await orc.sendWakeup(
            [],
            "HARD WARNING: hermit-dev surface validation failed",
            formatSelfTest(lastSelfTest),
          );
        }
      } catch (err) {
        lastSelfTest = {
          pass: false,
          checks: [{
            name: "registered-surface-validation-completed",
            pass: false,
            severity: "error",
            detail: String(err),
          }],
        };
        orc.log("error", formatSelfTest(lastSelfTest));
        await orc.sendWakeup(
          [],
          "HARD WARNING: hermit-dev surface validation failed",
          formatSelfTest(lastSelfTest),
        );
      }
    },
  );

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
      sig: 'await orc["hermit-dev.listScripts"]()',
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
      sig: 'await orc["hermit-dev.verifyScriptArgs"]()',
    },
  );

  orc.exposeFunction(
    PLUGIN_NAME + ".selftest",
    verifyRegisteredSurface,
    {
      description:
        "Exercise the registered hermit-dev functions and scripts against the live registries",
      params: [],
      sig: 'await orc["hermit-dev.selftest"]()',
    },
  );

  orc.exposeFunction(
    PLUGIN_NAME + ".status",
    hermitDevStatus,
    {
      description: "Report hermit-dev plugin registration and policy source state",
      params: [],
      sig: 'orc["hermit-dev.status"]()',
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
    policyReloadHeartbeat,
    "Re-inject AGENTS.md into coordinator context every two hours and report " +
      "a distinct failure when injection does not complete",
    {
      name: POLICY_RELOAD_WORKFLOW_NAME,
      restartable: HEARTBEAT_RESTARTABLE,
    },
  );

  orc.registerStartup(PLUGIN_NAME + ".startup", async function hermitDevStartup() {
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
      for (const legacyWorkflow of [
        LEGACY_PR_HEALTH_WORKFLOW_NAME,
        LEGACY_POLICY_RELOAD_WORKFLOW_NAME,
        LEGACY_SPECULATIVE_LAND_WORKFLOW_NAME,
      ]) {
        try {
          await orc.killWorkflow(legacyWorkflow);
        } catch (_err) {
          // Legacy workflows are absent in new sessions.
        }
      }
    }
    const result = await activateHermitDevPolicies("startup");
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
