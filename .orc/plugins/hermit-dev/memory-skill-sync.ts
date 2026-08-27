// ============================================================================
// Memory <-> skill sync heartbeat  (ORC session-store cross-check layer)
// ============================================================================
//
// STATUS: REVIEWABLE, NOT YET WIRED. This file is deliberately NOT imported by
// index.ts. Nothing runs until a coordinator (1) reviews it, (2) adds the import
// line documented under "WIRING" below, and (3) restarts ORC. It could not be
// typechecked or live-run in the authoring environment (orc.* is JS-only, and
// the live plugin is load-bearing), so treat the parser and the sessionMemories()
// shape as REVIEW POINTS, not proven.
//
// WHAT THE MECHANICAL LAYER ALREADY COVERS (no code change needed):
//   The FILE-BASED memory store (~/.claude/projects/<key>/memory/*.md) is what
//   the coordinator SKILLS mirror (sync-memory-skill.rs reads that store). The
//   tick-hub reminder `memory_skill_sync` (ci-hub/health/tick-hub.yaml) already
//   runs, every hour, inside the EXISTING operationalHealthHeartbeat workflow:
//     - lint-memory-skill-sync.rs         -> structural drift (skill<->memory)
//     - memory-skill-contradiction-scan.rs -> known-false-claim contradictions
//   and its ACTION line is already turned into an orc.sendWakeup() by
//   operationalHealthHeartbeat's actionableTickLines() filter. So bidirectional
//   file-store drift + contradictions are ALREADY reported-via-wakeup today.
//
// WHAT THIS MODULE ADDS (the genuinely orc-only piece):
//   `orc.sessionMemories()` exposes a SEPARATE store from the file-based one
//   (per measured fact: orc.sql cannot see memories; the API returns a formatted
//   markdown STRING numbered-list, no ids, no timestamps). A bash gate cannot
//   read that store. This heartbeat set-diffs the SESSION store against the
//   file store (obtained robustly from the scanner's `--list`), and hard-warns
//   when they diverge (a session memory with no file-store counterpart, or a
//   file-store memory absent from the session view) so the two do not silently
//   drift apart. It is REPORT-ONLY.
//
// !!! DELETION HAZARD (MEASURED) — read before ACTING on any proposal !!!
//   orc.sessionForget(index) is 1-BASED POSITIONAL and indices SHIFT after each
//   removal. If reconciliation deletes MULTIPLE session memories, apply the
//   deletions in DESCENDING index order or you WILL corrupt unrelated entries.
//   This module therefore ONLY REPORTS. It never calls sessionForget/Remember.
//
// WIRING (coordinator, after review):
//   1. In index.ts, add near the other imports/registrations:
//          import "./memory-skill-sync.ts";
//      (or the loader form this ORC build uses for sibling plugin modules).
//   2. Restart ORC so the new workflow + script register.
//   3. Verify with:  orc.hermitDevStatus() and that the workflow
//      `hermit-dev-memory-skill-sync-v1` appears in the workflow list.
//   4. Verify by TICK, not by presence: the workflow must be `alive` with
//      `crash_error: null` after at least one interval, and the module must have
//      survived a restart. "It appears in the list" is what let the sibling
//      workflows crash-loop unnoticed.
//   To DISABLE: remove the import and restart. No state is persisted beyond a KV
//   de-dupe key, so removal is clean.
//
// POST-1.0 REDUCED-CONTEXT RULE (see the header of index.ts for the measured
// evidence): the engine re-evaluates a workflow's module on every restart, under
// a capability preset that does NOT publish orc.registerScript / exposeFunction /
// registerStartup. An unguarded registration call at module scope therefore turns
// the first restart into a permanent crash-loop. The registrations at the bottom
// of this file are wrapped accordingly; keep any new side effect inside that
// guarded function.
// ============================================================================

// `orc` is an ambient global in the plugin eval context (same as index.ts).
// `wf` is typed loosely on purpose — index.ts already uses `{} as any` for the
// workflow options, so we avoid depending on a WfContext type that may not be
// exported to sibling modules.
declare const orc: any;

const MEMORY_SKILL_WORKSPACE_ROOT =
  (typeof orc !== "undefined" && orc.pluginDir && orc.pluginDir())
    ? orc.pluginDir() + "/../../.."
    : "$HOME/work/dev-hermit";

const MEMORY_SKILL_LIST_SCRIPT_NAME = "hermitMemorySkillList";
const MEMORY_SKILL_LIST_COMMAND =
  'cd "' + MEMORY_SKILL_WORKSPACE_ROOT + '" && ' +
  "./scripts/memory-skill-contradiction-scan.rs --list";

// Session store changes at coordinator cadence, not CI cadence: hourly is ample
// and matches the tick-hub `memory_skill_sync` reminder.
const MEMORY_SKILL_SYNC_INTERVAL_MS = 60 * 60 * 1000;
const MEMORY_SKILL_SYNC_WORKFLOW_NAME = "hermit-dev-memory-skill-sync-v1";
const MEMORY_SKILL_SYNC_ALERT_CACHE_KEY = "hermit-dev.memory-skill-sync-alert";

// Normalize a memory title/name for cross-store comparison: lowercase, collapse
// any non-alphanumeric run to a single hyphen, trim. This lets a session title
// ("KVM ptrace(2)->EPERM parity") match a file slug ("kvm-ptrace-eperm-parity")
// approximately. Deliberately lossy; divergence is coordinator-confirmed.
function normalizeMemoryKey(text: string): string {
  return String(text)
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
}

// Parse `orc.sessionMemories()`'s markdown STRING (numbered list, no ids). We
// accept `N.`, `N)`, and `- ` bullets; strip a leading bold/title marker. This
// is a REVIEW POINT: confirm against real output and tighten if needed.
export function parseSessionMemoryTitles(dump: string): string[] {
  const titles: string[] = [];
  for (const raw of String(dump || "").split("\n")) {
    const line = raw.trim();
    if (!line) continue;
    const m = line.match(/^(?:\d+[.)]|[-*])\s+(.*)$/);
    if (!m) continue;
    // Keep the human title; drop a trailing " — hook"/": desc" tail if present so
    // the key is the title, not the description.
    let title = m[1].replace(/\*\*/g, "").trim();
    const sep = title.search(/\s+[—–-]\s+|\s*:\s+/);
    if (sep > 0) title = title.slice(0, sep).trim();
    if (title) titles.push(title);
  }
  return titles;
}

// Parse the scanner's `--list` output: `name<TAB>slug<TAB>core|plain` per line.
function parseFileStore(
  stdout: string,
): Array<{ name: string; slug: string; core: boolean }> {
  const out: Array<{ name: string; slug: string; core: boolean }> = [];
  for (const raw of String(stdout || "").split("\n")) {
    const line = raw.trimEnd();
    if (!line) continue;
    const parts = line.split("\t");
    if (parts.length < 2) continue;
    out.push({
      name: parts[0],
      slug: parts[1],
      core: (parts[2] || "").trim() === "core",
    });
  }
  return out;
}

// Compute divergence both directions. A session memory matches a file memory if
// their normalized keys are equal OR one contains the other (titles are longer
// than slugs). Report-only; conservative substring matching favors NOT flagging.
function crossCheck(
  sessionTitles: string[],
  fileStore: Array<{ name: string; slug: string; core: boolean }>,
): { sessionOnly: string[]; fileOnly: string[] } {
  const fileKeys = fileStore.map((f) => ({
    key: normalizeMemoryKey(f.name),
    slugKey: normalizeMemoryKey(f.slug),
    slug: f.slug,
  }));
  const sessionKeys = sessionTitles.map((t) => normalizeMemoryKey(t));

  const matches = (a: string, b: string) =>
    a === b || (a.length > 3 && b.length > 3 && (a.includes(b) || b.includes(a)));

  const sessionOnly = sessionTitles.filter((_t, i) => {
    const sk = sessionKeys[i];
    return !fileKeys.some((f) => matches(sk, f.key) || matches(sk, f.slugKey));
  });
  const fileOnly = fileStore
    .filter((f) => {
      const fk = normalizeMemoryKey(f.name);
      const fs = normalizeMemoryKey(f.slug);
      return !sessionKeys.some((sk) => matches(sk, fk) || matches(sk, fs));
    })
    .map((f) => f.slug);

  return { sessionOnly, fileOnly };
}

export async function memorySkillSyncHeartbeat(wf: any): Promise<void> {
  await wf.loop(async () => {
    let sessionDump = "";
    let sessionErr = "";
    try {
      // MEASURED: async, returns a formatted markdown STRING (not structured).
      sessionDump = String(await orc.sessionMemories());
    } catch (err) {
      sessionErr = "sessionMemories() failed: " + String(err);
    }

    const listResult = await orc.scripts.hermitMemorySkillList() as {
      exitCode: number;
      stdout?: string;
      stderr?: string;
    };
    const listOk = Number(listResult.exitCode) === 0;
    const fileStore = parseFileStore(String(listResult.stdout || ""));

    // If either side is unreadable, the cross-check itself is DOWN -> warn once.
    if (sessionErr || !listOk || fileStore.length === 0) {
      const why = sessionErr ||
        (!listOk
          ? "memory-skill --list exit " + listResult.exitCode + ": " +
            String(listResult.stderr || "").trim()
          : "file store returned zero memories");
      const sig = "unreadable:" + why;
      if (orc.kvGet(MEMORY_SKILL_SYNC_ALERT_CACHE_KEY) !== sig) {
        await orc.sendWakeup(
          [],
          "HARD WARNING: memory<->skill sync cross-check is DOWN",
          "The session-store vs file-store consistency check could not run: " +
            why +
            "\nThis means memory drift is currently UNMONITORED on this axis. " +
            "The hourly tick-hub `memory_skill_sync` reminder still covers " +
            "file-store<->skill drift + contradictions independently.",
        );
        orc.kvSet(MEMORY_SKILL_SYNC_ALERT_CACHE_KEY, sig);
      }
      await wf.sleep(MEMORY_SKILL_SYNC_INTERVAL_MS);
      return;
    }

    const sessionTitles = parseSessionMemoryTitles(sessionDump);
    const { sessionOnly, fileOnly } = crossCheck(sessionTitles, fileStore);

    if (sessionOnly.length === 0 && fileOnly.length === 0) {
      orc.kvSet(MEMORY_SKILL_SYNC_ALERT_CACHE_KEY, "");
      await wf.sleep(MEMORY_SKILL_SYNC_INTERVAL_MS);
      return;
    }

    // De-dupe identical repeat wakeups.
    const signature = "so:" + sessionOnly.slice().sort().join(",") +
      "|fo:" + fileOnly.slice().sort().join(",");
    if (orc.kvGet(MEMORY_SKILL_SYNC_ALERT_CACHE_KEY) !== signature) {
      const body = [
        "The ORC session-memory store and the file-based memory store (which the",
        "coordinator skills mirror) have DIVERGED. This is REPORT-ONLY — the",
        "coordinator reconciles; this workflow never edits or deletes anything.",
        "",
        "SESSION-ONLY (in orc.sessionMemories(), no file-store counterpart) — " +
        sessionOnly.length + ":",
        ...sessionOnly.map((t) => "  - " + t),
        "",
        "FILE-ONLY (in memory/*.md, absent from the session view) — " +
        fileOnly.length + ":",
        ...fileOnly.map((s) => "  - " + s),
        "",
        "RECONCILE (proposal, apply manually):",
        "  * A FILE-ONLY memory that should be a session memory: orc.sessionRemember(...).",
        "  * A SESSION-ONLY memory worth keeping durably: write memory/<slug>.md + MEMORY.md.",
        "  * A stale SESSION-ONLY memory to drop: orc.sessionForget(index).",
        "",
        "!!! sessionForget(index) is 1-BASED POSITIONAL and indices SHIFT after each",
        "    removal. Delete in DESCENDING index order, or you corrupt other entries.",
        "",
        "Matching is approximate (normalized title<->slug); confirm each before acting.",
        "Run ./scripts/memory-skill-contradiction-scan.rs for the file-store<->skill view.",
      ].join("\n");
      await orc.sendWakeup(
        [],
        "memory<->skill sync: session/file stores diverged (" +
          sessionOnly.length + " session-only, " + fileOnly.length +
          " file-only) — REPORT-ONLY",
        body,
      );
      orc.kvSet(MEMORY_SKILL_SYNC_ALERT_CACHE_KEY, signature);
    }
    await wf.sleep(MEMORY_SKILL_SYNC_INTERVAL_MS);
  });
}

// --- registration (runs only once this module is imported by index.ts) ---
// Every side effect lives in here so the reduced-context guard below can skip it
// wholesale on a workflow-restart re-evaluation. See the POST-1.0 note above.
function registerMemorySkillSyncSurface(): void {
  orc.registerScript(MEMORY_SKILL_LIST_SCRIPT_NAME, {
    script: MEMORY_SKILL_LIST_COMMAND,
    description:
      "List file-based memory store (name<TAB>slug<TAB>core) for session-store cross-check",
    timeoutSec: 60,
  });

  orc.workflow(
    memorySkillSyncHeartbeat,
    "Cross-check the ORC session-memory store against the file-based store; " +
      "hard-warn (report-only) on divergence",
    {
      name: MEMORY_SKILL_SYNC_WORKFLOW_NAME,
      // Matches index.ts: back a restart off instead of burning three attempts
      // in under a second and leaving the heartbeat silently dead.
      restartable: { maxRestarts: 100, backoffMs: 5_000, maxBackoffMs: 120_000 },
    },
  );
}

// Swallow only the host's absent-name dispatch failure — that is the reduced
// workflow-restart context, where this module is being re-evaluated purely to
// resolve memorySkillSyncHeartbeat, which needs no registration. Anything else
// is a real bug and is re-thrown.
try {
  registerMemorySkillSyncSurface();
} catch (err) {
  const message = String((err && (err as { message?: unknown }).message) || err);
  if (message.indexOf("is not a function") === -1) {
    throw err;
  }
}
