[hermit2, sub-hermit-agentctl-014, claude-opus-5-5, devbig014, role=impl]

# Coordinator inbox and wakeup planning

Status: planning document, 2026-09-29. Author: sub-hermit-agentctl-014 (agentcloud Claude Code
session on devbig014). Everything marked **TESTED** was run on devbig014 on 2026-09-29 and the
commands and output are in section 4. Everything else is read from source (with file:line) or from
public documentation (with URL), and says so.

## Background and terms

The dev-hermit coordinator is one long-running agent (today a Claude Code TUI in herdr pane `wJ:p1`)
that dispatches work to many other agents and reports to the owner over Google Chat. This document is
about the reverse direction: **how the coordinator finds out that a subagent finished, got stuck, or
said something important**, and how several such signals plus owner messages reach it without being
lost, duplicated, or typed into the middle of its work.

Terms used below:

- **Coordinator**: the agent that assigns work and talks to the owner.
- **Native subagent**: a subagent created by the coordinator's own harness (Claude Code `Agent` tool
  or Workflow). The harness tells the coordinator automatically when one finishes (a
  "task-notification"). These are cheap to supervise but die with the coordinator's session.
- **External subagent** (or worker): a persistent agent in its own herdr pane or agentcloud session,
  started by `agentctl` or by hand. It survives coordinator restarts, but today nothing tells the
  coordinator when it finishes; the coordinator has to go and look.
- **Idle**: the agent finished its turn and is waiting for input. herdr reports it as `idle` or `done`;
  agentcloud as a settled run; Claude Code as `status: idle`.
- **Blocked**: the agent is waiting on something only a person or the coordinator can give (a
  permission dialog, a question). herdr reports `blocked`.
- **New output**: transcript content the coordinator has not yet read, measured from a stored
  **read cursor** (a byte offset into a transcript file, or a journal sequence number).
- **Inbox**: a durable queue of notifications addressed to the coordinator.
- **Delivery**: getting inbox content in front of the coordinator's model. Mechanisms differ in whether
  they name the sender, whether they can arrive mid-turn, and whether they are exactly-once.
- **herdr**: the terminal multiplexer that hosts the agent panes. It classifies each pane's agent
  state from screen rules and exposes a socket API.
- **agentctl**: our subagent manager in agent-utils (`rs/agentctl`, Rust; a Python predecessor lives in
  `py/agentctl`).
- **agentcloud**: Meta's hosted agent-session service; `agentcloudctl` is its CLI. This document's
  author runs on it.

## Recommendations in one screen

1. **Build `agentctl watch` + `agentctl inbox`**: a per-coordinator watcher that turns subagent state
   changes into durable inbox entries, and one delivery step that renders all pending entries as one
   batch. The watcher takes events from herdr's push API and from `agentcloudctl fleet --watch`, and a
   30 s reconciliation sweep catches anything the event stream missed.
2. **The watcher detects; the coordinator decides.** Like ORC, it does not assign work. It tells the
   coordinator "these agents are idle, here is what they last said", once per idle edge, with
   exponential re-reminders while nothing changes.
3. **Deterministic first, cheap model second.** Tier 0 is the agent's last assistant message of the
   turn plus its status file's `BLOCKED ON:` line. That is free and usually *is* the result. Tier 1 is
   a Haiku summary of the transcript delta, keyed by cursor range and cached. It never replaces the
   raw text; each entry carries the range so the coordinator can read the source.
4. **Delivery adapter per coordinator type**, best first:
   - agentcloud coordinator: `agentcloudctl notify --mode cli-script --idempotency-key …`
     (TESTED: attributed, delivered mid-turn, exactly-once per key).
   - local Claude Code coordinator: agentctl's existing verified-typing path (`send`/`drain`), used
     only when the coordinator pane is idle and the owner has not typed recently. Worker-to-coordinator
     messages should move from raw `herdr agent prompt coord` to Claude Code's native cross-session
     `SendMessage` (TESTED on this host: attributed, queued natively, starts a turn when idle).
   - Claude `--channels`: **not usable** under the Meta wrapper (TESTED, section 4.1).
5. **Owner first, never merged.** Owner Chat messages keep their own path (the chat bridge, owned by
   the `chat-bridge` agent). The inbox yields to it and shares its per-pane delivery lock, so the two
   never type into the coordinator at once.
6. **Keep a slow heartbeat.** A Claude Code cron or `/loop` at 20–30 min stays as the safety net (ORC
   and Gas Town both keep one). The 10-minute scrollback dump can retire once the watcher is proven.

## 1. What we have today

### 1.1 How the coordinator learns about subagents now

| Mechanism | What it does | Gaps |
|---|---|---|
| `temp/coord-scrollback-delta.py` on a 10-min Claude cron | For every `herdr agent list` pane: reads 600 lines (`herdr pane read --source recent-unwrapped`), drops the bottom 10 as redraw noise (`FOOTER_LINES = 10`, line 16), finds the previous read's last 12 non-blank lines (`ANCHOR_LINES = 12`, line 17), prints up to 25 new lines per pane. State: `~/.local/state/coord-scrollback/<pane>.txt` (line 15). | Pull-only, 10-min latency. Scrollback is wrapped, redrawn terminal text, not the transcript. The anchor search fails silently when output scrolls more than 600 lines (the whole body is then "new"). Every agent is dumped whether or not anything matters. |
| Agents run `herdr agent prompt coord "$(cat msg)"` with a self-written `[name -> coord HH:MMZ]` prefix | Types the text into the coordinator's composer and submits it. | Unauthenticated (anyone can write any prefix). Lands as a *user* turn. If the coordinator is mid-turn, Claude Code queues it as typed input. Nothing batches several agents. No retry record. |
| Native subagents (Agent tool, Workflow) | Harness task-notification on completion. | Only for native subagents; they die with the session. |
| Owner Chat | Chat bridge (`agentctl chat run`, section 1.3). | Owned by chat-bridge; reliability work in https://github.com/rrnewton/agent-utils/issues/175. |

### 1.2 agentctl (Rust, agent-utils `rs/agentctl`, read at agent-utils 138dcfd5)

- Commands (`cli.rs:67-144`): `start, adopt, move, stop, list, status, send, drain, read, wait, goal,
  bind-session, attach, pause, resume, chat, profiles, skill, capabilities, quickstart, userguide`.
- **There is no subagent daemon.** The only long-running agentctl process is the chat service
  (`agentctl chat run`). `wait` blocks one caller until one agent is `idle`/`done` by polling herdr
  (`POLL_INTERVAL = 250 ms`, `agent.rs:31`); for agentcloud agents it uses
  `agentcloudctl wait --until settle`.
- Send path: each prompt is journaled in `.agentctl/<agent>/inbox/`, moved to `inflight/` when
  injection starts, and dropped on confirmed "working" or moved to `failed/` when the outcome is
  ambiguous (states in `agent.rs:65-74`). Typing is verified against herdr's screen state before it
  counts as delivered. For agentcloud targets `send` uses `agentcloudctl send` for the agent's own
  session, or `send-message` when a sender session is named. That machinery can be reused for
  coordinator delivery.
- `read` is a fresh snapshot each time. **No per-reader cursor exists anywhere.**
- The chat service already subscribes to herdr push events for the coordinator pane
  (`chat_events.rs:163-186`: `pane.output_matched` for reply fences and
  `pane.agent_status_changed` for `idle`/`done`). That is the same primitive the watcher needs,
  already in production.

### 1.3 Chat bridge (read-only for this work)

Owner messages enter through a subscription plugin, are deduplicated and admitted into
`requests/<key>.json`, delivered with agentctl's verified send, and replies are captured from herdr
output between reply fences. It has its own durable state (`bridge.json`, `checkpoint.json`,
`requests/`, `replies/`, `committed/`, `retired/`, `gap.json`). The inbox must not change this path;
section 5.7 describes the one shared point, the per-pane delivery lock.

### 1.4 Signals available on this host (all TESTED, section 4)

| Source | What it gives | Push or poll |
|---|---|---|
| herdr socket `events.subscribe` | `pane.agent_status_changed` (idle, working, blocked, done, unknown), `pane.output_matched` (regex on output), `pane.scroll_changed`, `pane.created/closed/exited` | push |
| `herdr agent list` | per-pane `agent_status`, `revision`, `state_change_seq` | poll |
| `claude agents --json` | every local Claude Code session: `pid`, `sessionId`, `name`, `status` busy/idle (interactive) or `state` (background) | poll |
| `/proc/<pid>/environ` | `HERDR_PANE_ID` of a Claude process, so session ↔ pane mapping is exact | poll |
| Claude transcript `~/.claude/projects/<proj>/<sessionId>.jsonl` | the real transcript; byte offset is a natural cursor | file |
| `agentcloudctl fleet [--watch]` | per session `running`, `phase`, `last_seq`, `last_activity_unix_ms`, narrator `status` row | push with `--watch` |
| `agentcloudctl wait` | any-event wait on one session | push (blocking) |
| agentcloud `--narration` | a cheap sidecar model writes status row, activity line and recap from the journal | server-side |

## 2. How ORC's wakeup loop works

ORC source at `/home/newton/fbsource/fbcode/orc`. The engine provides mechanism; the policy is in
TypeScript workflow plugins.

**What wakes the coordinator.** The engine loop (`orc-engine/src/engine/engine_run.rs`) is a
`tokio::select!` with `biased;` (line 495) over cancellation, control commands, submitted
messages (line 539), always-on commands, routed messages, session exit, and a poll tick
(`AGENT_POLL_SECS = 2`, line 54; `MIN_LOOP_INTERVAL = 1 s`, line 57). Prompts and wakeups share the
**submit channel, which is idle-only by construction**: `run_turn()` never receives the receiver, so
a wakeup can never interrupt a turn (`core.rs:12-19`, `engine_run.rs:404`). `/pause` maps to
`ControlCmd::PauseWakeups` (`dispatch.rs:196`).

**The idle re-dispatch policy** is `plugins/stdlib-core/workflows/work-assignment.ts:7-33`:

```ts
await wf.loop(async () => {
  await wf.sleep(5000);
  if (await wf.turnInProgress()) return;
  if (await wf.userRecentlyTyping(5000)) { ... return; }
  const agents = await orc.listAgents();
  const idle = idleAgents(agents, await readUnmanagedAgentNames());
  if (!idle.length) return;
  const hints = await workAssignmentHint(idle);
  if (await wf.turnInProgress()) { ... return; }   // re-check after building context
  await orc.sendWakeup(names, 'Agents are idle and need direction', hints);
});
```

- **Idle** (`routing.ts:93-105`): an unmanaged agent with status `needs-input` **and no
  `current_task`** ("A mid-task question needs an answer, not a new assignment"). The status comes
  from the dg agent manager (`orc-engine/src/polling.rs:411-445`); ORC does not parse transcripts.
- **Picking work**: ORC does not pick for the coordinator. `workAssignmentHint` adds ready TaskGraph
  rows (`routing.ts:27-35`: `ready` view, open, unowned, `ORDER BY priority ASC, impact DESC LIMIT 10`)
  and the coordinator's model decides.
- **Delivery**: `orc.sendWakeup(names, short, long)` → `handle_send_wakeup_js`
  (`orc-engine/src/effects/main_loop.rs:40-127`) reads the last 30 lines of each named agent's pane
  (`IDLE_AGENT_READ_LINES = 30`, `polling.rs:448`) in parallel with a 4 s total budget
  (`TOTAL_PANE_READ_TIMEOUT_SECS`, `main_loop.rs:33`; a timeout substitutes "(pane read timed out)"),
  then queues a `Block::Wakeup` onto the idle-only submit path. The message format is
  `polling.rs:812-848`: "Idle agents need attention. Actions: `dg send <agent> …`, `dg free <agent>`",
  then optional TaskGraph status and one fenced pane excerpt per agent.
- **Races**: two `turnInProgress()` checks (before and after the slow context build) plus the idle-only
  channel prevent injecting into a running turn. The user-typing check avoids clobbering the human.
  There is **no per-idle-edge dedup** in work-assignment: while an agent stays idle, a new wakeup can be
  queued every 5 s the coordinator is idle. The loop relies on the coordinator acting (or `dg free`).
  The stuck-task workflow does throttle: at most 5 tasks, each re-reported no sooner than 60 s
  (`routing.ts:125-136`).
- **Heartbeat**: plugins add a fixed progress loop, e.g. `plugins/10x-orc/config.ts:166-181` sleeps
  300 s and calls `sendWakeup([], "Progress Check", …)`, restartable with backoff.
- **Summarization**: none. ORC passes raw pane excerpts.

**Compared with us.** ORC's coordinator is woken by its own engine, on an idle-only channel, within
about 5 s of a worker going idle. Ours is woken only by the 10-minute cron, by owner Chat, or by a
worker typing into it. The pieces to copy are: the idle definition (idle *and* not mid-task), the two
turn-in-progress checks, the typing guard, and bounded raw excerpts. Unlike ORC, we should dedup per
idle edge. Our coordinator is an LLM whose every wakeup costs a turn, and ORC's 5 s repeat is
tolerable only because its coordinator is expected to act immediately.

## 3. Related work

Evidence: a research subagent read the source of Gas Town (Go module snapshot of
`github.com/steveyegge/gastown` at commit 649b832b7672, 2026-07-23), Amazon CLI Agent Orchestrator
2.5.0 (PyPI), claude-squad v1.0.20, autogen-agentchat 0.7.5, langgraph 1.2.12 and claude-flow
3.48.0, plus the live Claude Code docs (`code.claude.com/docs/en/*`) and Codex docs
(`developers.openai.com/codex/*`). GitHub, Medium, conductor.build, imbue.com and vibekanban.com were
unreachable through the proxy; claims about Conductor, Sculptor and Vibe Kanban are **unverified**.

- **Gas Town** (https://github.com/steveyegge/gastown). Roles: Mayor (global coordinator), Deacon
  (daemon patrol), Witness (per-repo pit boss that watches workers), Refinery (merge queue), Polecats
  (workers). Mail is stored as beads. `gt nudge` has three modes:
  - `wait-idle` (default): poll the last 5 pane lines every 200 ms for the prompt and the absence of
    "esc to interrupt"; two consecutive idle polls count as idle; after 15 s fall back to the queue.
  - `queue`: a JSON file per nudge, named by timestamp for FIFO. Draining claims each file by atomic
    rename; orphaned claims return after 5 min; TTL is 30 min (2 h urgent); depth cap 50.
  - `immediate`: tmux send-keys under an `flock`, because concurrent nudges garble input.

  Claude workers empty the queue from a `UserPromptSubmit` hook, which renders everything as one
  system-reminder with urgent items first. The Witness patrol waits on the beads activity feed, backs
  off 30 s → 5 min when nothing happens, and runs cheaper "EFFORT: reduced" cycles when idle. A Go
  daemon heartbeat every 3 min is the safety net. Its stuck thresholds are deliberately generous
  (30 min without progress) because "false positives … are worse than slow detection". There is no
  transcript summarization; patrol roles can simply run on Haiku or Sonnet.
- **Claude Code native.**
  - Agent teams: a JSON mailbox per agent; the lead is told automatically when a teammate stops,
    including its final answer.
  - Cross-session messaging: `ListAgents`/`SendMessage` over per-session sockets. Messages are read
    between tool calls, or start a turn when the receiver is idle. `notify_when_idle` gives a one-shot
    idle notice. There is a 50-message queue cap, repeat dedup and a per-sender rate limit.
  - Hooks: `Stop`, `Notification` (`idle_prompt`), and `asyncRewake`.
  - Monitor tool, `/loop` and cron (fire only between turns, no catch-up).
  - Channels: MCP push, batched per turn.
  - Agent view: one-line row summaries written by a Haiku-class model at every turn end, refreshed
    every few minutes during long turns, with free 15 s updates from raw output in between.
  - `claude agents --json` is documented as the supported way for external supervisors to read state.
- **Amazon CLI Agent Orchestrator** (https://github.com/awslabs/cli-agent-orchestrator).
  - Idle detection: `pyte` renders tmux output; status is judged after 0.2 s of quiescence with
    per-provider regexes, and ready states are sticky so redraws do not flap.
  - Inbox: messages are PENDING in a database and delivered only when the receiver is IDLE or
    COMPLETED. Three paths reach that gate: an immediate attempt, a 5 s log watchdog, and a 30 s
    reconciliation sweep, which exists because an already-idle receiver produces no output to trigger
    the others.
  - Delivery: one message per idle cycle by default; a full drain groups contiguous runs from the same
    sender. Messages are marked DELIVERED *before* the bracketed paste, to avoid duplicates.
  - It has a herdr backend.
- **Codex CLI.** The app-server pushes `thread/status/changed` (idle, active, waitingOnApproval) and
  `turn/completed`. Input can be added mid-turn with `turn/steer` (which needs `expectedTurnId`) or
  without starting a turn via `thread/inject_items`. `notify` runs a program on turn completion.
- **claude-squad**: pane hash every 500 ms (unchanged means ready). No messaging.
- **Magentic-One**: an LLM progress ledger per step with a stall counter (max 3) that forces re-planning.
- **LangGraph**: checkpointed `interrupt()`, resumed by interrupt id; nodes re-run from the start.

| System | Idle signal | Delivery into coordinator | Batching / ordering | Cheap-model summary |
|---|---|---|---|---|
| ORC | dg status `needs-input` and no task, 5 s tick | idle-only engine channel | per wakeup; excerpts of 30 lines | none |
| Gas Town | pane prompt, 2 polls × 200 ms; self-reported heartbeat | tmux keys when idle, else file queue drained by hook | FIFO files, urgent first, TTL, cap 50 | Haiku/Sonnet as patrol model |
| CAO | screen quiescence + regex | paste when IDLE; 30 s sweep | one per cycle or grouped by sender | none |
| Claude Code | native busy/idle | cross-session queue, teammate mailbox, channels | queue cap 50, grouped per turn | Haiku agent-view rows |
| Codex | `thread/status/changed` | `turn/steer`, `inject_items` | per turn | none |
| **Proposed** | herdr push + fleet watch + 30 s sweep | agentcloud notify, or verified typing when idle | one batch render, priority classes, per-agent coalescing | tier 0 last message, tier 1 Haiku delta |

## 4. Experiments (devbig014, 2026-09-29)

All scratch material was under `/tmp/sa014-channel/` and `/tmp/sa014-sum/`. Scratch tmux sessions
were killed afterwards. The coordinator was never used as a test target.

### 4.1 Claude `--channels` under the Meta wrapper: does not work

Scratch MCP server `chan.py` (stdio, declares `capabilities.experimental["claude/channel"] = {}`,
pushes `notifications/claude/channel` with `content` and `meta` 10–40 s after `initialized`), loaded
with `--dangerously-load-development-channels server:inboxtest --mcp-config mcp.json
--strict-mcp-config`. Claude Code 2.1.284/2.1.285.

| Mode | Upstream | Result |
|---|---|---|
| `claude -p --input-format stream-json` (the agentcloud driver's shape) | AI Gateway, Anthropic upstream | server connected; both notifications sent (server log); **no turn, no delivery** |
| interactive, default env | AI Gateway, **Vertex** upstream | footer: `Channels are not available on Bedrock, Vertex, or Foundry`; no delivery |
| interactive, agentcloud-equivalent env | Anthropic upstream | footer: `Channels are not currently available`; debug log: `MCP server "inboxtest": Channel notifications skipped: channels feature is not currently available` |

Cause, read from the 2.1.285 binary: the channel gate returns `disabled` when
`x("tengu_harbor", false)` is false. That is a GrowthBook feature flag, and this install has no
cached GrowthBook features (non-essential traffic is disabled) and no environment override path
(`getEnvironmentOverrides()` returns `null` in this build). Managed settings also do not set
`channelsEnabled`. **Conclusion: Claude channels are unavailable to us.** Enabling them would need
the vendor flag and a Meta managed-settings change, not anything we control.

### 4.2 agentcloud `notify`: works, exactly-once, mid-turn

```
agentcloudctl notify --session "$AGENTCLOUD_SESSION_ID" --mode cli-script \
  --idempotency-key sa014-notify-probe-1 --text "sa014 notify probe (cli-script mode): token HERON-7. ..."
# rc=0, stdout: op-sa014-notify-probe-1
# same command, same key, different text:
# rc=0, stdout: op-sa014-notify-probe-1   (no second delivery observed)
```

The message appeared **inside the author's running turn** as
`<session-message from="cli-script"> … </session-message>`, i.e. at peer trust, attributed, without
waiting for the turn to end. `--mode system-notification` (the default) would deliver it as an
operator system-reminder instead. This is the best delivery primitive available when the coordinator
runs on agentcloud.

### 4.3 Claude Code cross-session messaging: works locally

- `ListAgents` from this session listed 10 peer sessions on the host. `claude agents --json` returned
  13 (11 interactive with `pid`, `sessionId`, `status`, 2 background with `state`).
- Mapping by `/proc/<pid>/environ` `HERDR_PANE_ID`: coordinator `wJ:p1` = pid 1142801 = session
  name `dev-hermit-e4`; chat-bridge `wJ:p5G` = pid 2293495; and so on for every herdr Claude pane
  (one interactive session, pid 3590499, has no herdr pane).
- Session names are auto-generated (`dev-hermit-e4`) unless the session is launched with
  `claude -n <name>`. agentctl should pass `-n <agent-name>` so that the name *is* the address.
- `SendMessage(to="sa014-rx-default", message=…, notify_when_idle=true)` to an idle scratch session
  (manual permission mode): the receiver started a new turn at once and showed
  `Message from @dev-hermit-de: …`. Its model treated the text as peer input and asked its human before
  acting. That is the right trust level for notifications and the wrong one for commands.
- The idle notice did not come back, because it is delivered only within the same permission class.
- **Not tested**: delivery into a `--dangerously-skip-permissions` receiver (the coordinator's mode).
  The auto-mode classifier refused launching a bypass-mode scratch session, and the coordinator itself
  was not used as a target. The tool documentation says a receiver in a different permission mode
  *holds* messages for its user's approval. If that applies, worker → coordinator SendMessage would sit
  unapproved while the owner is away. This must be tested before recommending it fleet-wide
  (open question Q1).

### 4.4 herdr push events

`events.subscribe` with one `{"type":"pane.agent_status_changed","pane_id":P}` per pane from
`herdr agent list` (11 panes) was accepted (`subscription_started`). No status transitions occurred in
the 90 s window, so this run proves only that the subscription is accepted. The chat service uses the
same subscription in production (`chat_events.rs:175-181`). Subscriptions are per pane, so the watcher
must also follow `pane.created` / `pane.closed`.

### 4.5 Cheap-model delta summaries

Source: the chat-bridge worker's transcript (18.0 MB JSONL). Cursor = byte offset; the delta is the
last 1.5 MB, cut at the last complete line.

- A 40-line extractor keeps assistant prose, tool-call one-line descriptions, non-meta user inputs and
  tool errors. Result: **1,500,000 bytes → 43 units, 4,448 bytes.** Most raw bytes were `attachment`
  rows (522 of the records). That tail had no assistant prose at all; the worker was mid-task, and
  prose appears mainly at turn end.
- `claude -p --model haiku --strict-mcp-config --disable-slash-commands --tools ""` over the digest:
  **34 s wall per call**, almost all CLI startup (SessionStart hooks, plugin load). The model time is
  small.
- Prompt design matters. A prompt that allowed "reply NOTHING-NEW if nothing matters" returned
  `NOTHING-NEW` even though the digest clearly showed active work, a false negative. A fixed
  three-line contract was accurate:

```
NOW: Investigating the chat bridge's match-event capture flow and validate test status to prepare for reply-loss defect fixes.
NEW: none (validate steps still running; differential step shows a failure under review)
NEEDS: Rust test step and differential step completion; clarification on whether the Python shard failure blocks part 1 commit or is resolvable in part 2.
```

Implications: summaries must be asynchronous and cached (34 s is too slow to sit on a delivery path),
must never be the only signal, and the prompt must not offer an "ignore" escape hatch. A direct
gateway API call instead of `claude -p` would remove most of the 34 s. For agentcloud workers, the
existing narrator (`create --narration`) already produces this server-side.

### 4.6 Delivery-path finding about this session

This agentcloud session has **no herdr pane** (it is not in `herdr agent list`), although herdr is
reachable from inside it. `herdr agent prompt coord` therefore works *from* it, but nothing can
`herdr agent prompt` *into* it. Anything that supervises agentcloud workers must use `agentcloudctl`
(`notify`, `send-message`, `wait`, `fleet --watch`), not herdr. The session also runs with
`internet_mode=disabled`, so its GitHub traffic depends on `herdr-run`. At 21:09Z that route refused
because the `agent-cmds` workspace was at its 32-pane cap with no tab provably finished.

## 5. Design: `agentctl watch` and `agentctl inbox`

### 5.1 Requirements

From the owner:

- Alarms when subagents go idle or have new responses, supported in agentctl.
- Deterministic delivery into a coordinator inbox, with batching and ordering across several
  subagents plus the owner.
- Cheap-model summaries of transcript deltas keyed by read position.
- Coordinators must not freeze on prompts: blocked states are surfaced, and the separate perm-hook
  work converts permission dialogs into auto-denials.
- Implementation work moves to external persistent workers, while short-lived coordination helpers
  stay native. This makes the inbox the thing that gives external workers the free completion
  notifications native subagents already have.

Non-goals: assigning work automatically; replacing the chat bridge; any change to the live
coordinator path without asking coord.

### 5.2 Components

```
      herdr events.subscribe ─┐
agentcloudctl fleet --watch ──┼─> agentctl watch ──> inbox/<coordinator>/ ──> agentctl inbox deliver ──> coordinator
   30 s reconcile sweep ──────┘      (detect,           (durable FIFO,         (render one batch,
   (herdr agent list,                 cursor,             coalesce,              pick adapter,
    claude agents --json,             tier-0/1)           priority)              ack on success)
    fleet)
```

One watcher per coordinator, run as a user systemd unit next to the chat service. Its state lives
under the agentctl registry (`.agentctl/.watch/<coordinator>/`), so it follows the registry the
coordinator already uses (`.agentctl` for herdr agents, `.agentctl-cloud` for agentcloud).

### 5.3 Event model

| Event | Trigger | Priority |
|---|---|---|
| `blocked` | herdr `blocked`; agentcloud run waiting on input | P1 |
| `idle` | edge working → idle/done, stable for ≥ 3 s (Gas Town's two-poll rule) | P2 |
| `exited` | pane closed/exited, session ended, pid gone | P1 |
| `progress` | while working: delta since cursor exceeds a threshold (e.g. 64 KB of transcript or 20 min since last progress entry) | P3, off by default |
| `still_idle` | an idle agent nobody addressed: re-remind at 30 min, then 60 min, then 120 min, max 3 | P3 |

Owner messages are P0 but are not produced by the watcher. They stay on the chat bridge.

Dedup rule: at most one live `idle` entry per agent. A newer idle edge replaces the older entry and
extends its cursor range. A `working` edge after an undelivered `idle` cancels it: the agent picked
something up on its own, and the coordinator does not need to hear about it.

### 5.4 Read cursors

Per (coordinator, agent): `{source, path_or_session, delivered_to, acked_to, last_edge_seq}`.

- Claude herdr panes: `claude agents --json` gives pid → `sessionId`; `/proc/<pid>/environ`
  `HERDR_PANE_ID` gives pid → pane, path = `~/.claude/projects/<proj>/<sessionId>.jsonl`, offset = bytes. After
  `/compact` or a resume the session id changes; detect that by pid → session re-resolution and start
  a new cursor.
- Codex panes: the rollout JSONL under `~/.codex/sessions/` (resolution to be confirmed in the prototype).
- agentcloud: journal `last_seq` from `fleet`; content via `agentcloudctl history`/`tail`/`output`.
- Fallback (unknown harness): herdr pane `revision` plus a line-anchored scrollback read, the current
  script's method, marked `approximate`.

`delivered_to` advances when a batch is handed to an adapter successfully. `acked_to` advances when the
coordinator runs `agentctl inbox ack` (or reads the agent with `agentctl read --since-cursor`). A crash
between them re-delivers, so delivery is at-least-once with idempotency keys wherever the adapter
supports them.

### 5.5 What an entry says

```
[idle] kvm (codex, wJ:p38) idle since 21:04Z (working 42 min)
  last message: "Pushed 3f2a9c1 to reverie PR https://github.com/.../pull/628; local validate green (412 s)."
  status file:  BLOCKED ON: nothing
  delta: transcript bytes 18,011,745..18,402,110 (390 KB)   read: agentctl read kvm --range 18011745:18402110
```

- **Tier 0 (always, free):** last assistant text of the turn (≤ 600 chars), the first line of
  `temp/<agent>-status.md` if it exists, error count in the delta, and the cursor range.
- **Tier 1 (optional, async):** a Haiku NOW/NEW/NEEDS summary. It is triggered when tier 0 has no
  final prose, or the delta is over 256 KB, or the entry is `blocked`. It is cached under
  `.watch/summaries/<sha256(path,start,end)>.json` and attached if ready before delivery; never waited
  for longer than the delivery window.

### 5.6 Batching and ordering

- One delivery = one rendered batch: P1 entries first, then P2, then P3, each group in event-time
  order. It is capped at about 3 KB; any overflow becomes "N more: `agentctl inbox list`". This follows
  Gas Town's single system-reminder and Claude channels' "delivered together on the next turn".
- **Delivery window:** at most one batch per 60 s for P2/P3. P1 goes immediately, subject to the
  coordinator-idle check.
- **Coordinator-idle check (ORC):** the coordinator pane is idle, the owner has not typed in the last
  5 s (herdr pane input activity), and no chat-bridge delivery is in flight. Re-check after rendering.
- **Queue hygiene (Gas Town):** timestamp-named files, claim by rename, a stale-claim return after
  5 min, TTL 24 h for P2/P3 (an idle agent that is still idle is re-derived by the sweep anyway), and
  depth cap 200.

### 5.7 Delivery adapters

| Coordinator | Adapter | Attribution | Mid-turn | Exactly-once |
|---|---|---|---|---|
| agentcloud session | `agentcloudctl notify --mode cli-script --idempotency-key inbox-<coord>-<batch>` | "cli-script" (peer) | yes (TESTED) | per key (TESTED) |
| local Claude TUI (herdr) | agentctl verified send (`inbox/`→`inflight/`→`failed/`) with `message_id = inbox-<batch>`, only when idle | text prefix only (unauthenticated) | no, waits for idle | agentctl refuses a reused message id; ambiguous typing is quarantined, not retried |
| local Claude TUI, from Claude workers directly | Claude `SendMessage` to the coordinator's session name | native `from=` | between tool calls | native dedup; **bypass-mode hold untested (Q1)** |
| Codex coordinator | herdr verified send when idle; later app-server `thread/inject_items` | prefix | inject_items: yes | by message id |
| any | print to stdout (`agentctl inbox render`) for a cron/`/loop` tick to read | n/a | n/a | n/a |

Rejected: Claude `--channels` (section 4.1); `herdr agent prompt` without the idle check and without
the agentctl journal, which is what the fleet does today.

**Shared lock with the chat bridge.** Both the chat service and `inbox deliver` inject into the same
pane. They must take the same per-pane delivery lock, and the owner's message always wins the tie.
This is the one point that touches chat-bridge's area. The inbox work must not change
`chat_runtime`/`chat_service` without chat-bridge's agreement; the first prototype stays out of that
code and delivers only through agentcloud notify or print.

### 5.8 Failure handling

| Failure | Handling |
|---|---|
| herdr event stream drops or restarts | the 30 s sweep compares `state_change_seq`/`agent_status` from `herdr agent list` with last-seen, synthesizes missed edges, and records `source: sweep` |
| agent idle before the watcher started | the sweep emits one `idle` for every idle agent with unacked output at startup |
| coordinator never becomes idle | P1 still waits for idle on typed paths; on agentcloud it is delivered mid-turn. `inbox list` always shows the backlog |
| delivery ambiguous (typed, no working confirmation) | quarantine to `failed/` as agentctl does today; the next batch says "1 earlier batch may have been delivered" |
| watcher dies | systemd restart; state is durable; the Claude cron heartbeat still runs |
| summarizer slow or failing | tier 0 is delivered alone; the summary failure is noted in the entry |
| false idle during a long tool call | the ≥ 3 s stability rule plus herdr's own classification; an `idle` followed within the window by `working` is cancelled |

### 5.9 How this changes the coordinator's workload

- External workers become as cheap to supervise as native ones: the coordinator gets one batch naming
  who finished and what they said, instead of reading every scrollback every 10 minutes.
- The coordinator's cron can drop from "dump all scrollback every 10 min" to "run `agentctl inbox
  render` every 20–30 min as a safety net".
- Worker → coordinator messages can migrate from raw `herdr agent prompt` to `agentctl inbox post
  --from <agent>` (deterministic, queued, batched, idle-gated). This needs no owner decision: the owner
  said agents may keep messaging the coordinator, and this changes only the transport.

## 6. Prototype plan (agent-utils)

Each step gets an adversarial GOALPOST review before it is pushed to agent-utils main, and runs
`make validate` bare (not piped). No step touches the live coordinator, the chat service, or bridge
state without asking coord first. Per agent-utils policy the code names no consuming project, and the
tool gets `quickstart`, `userguide` and `RELATED_WORK.md`.

1. **Inbox core** (`rs/agentctl`, new module `inbox.rs`): durable queue (file per entry, claim by
   rename, TTL, cap), coalescing rules from 5.3, priority rendering from 5.6, cursor store from 5.4,
   and the CLI `agentctl inbox post|list|render|ack`. Unit tests for coalescing, ordering, claim races,
   TTL, and render caps. No network, no herdr.
2. **Watcher** (`agentctl watch --coordinator NAME`): herdr `events.subscribe` (reusing
   `chat_events.rs` framing), the 30 s sweep over `herdr agent list`, the Claude pid/session resolution,
   tier-0 extraction from Claude JSONL, and `fleet --watch` for agentcloud agents. Tests use a fake herdr
   socket and fixture transcripts.
3. **Delivery** (`agentctl inbox deliver --via agentcloud-notify|print`): the agentcloud adapter uses
   idempotency keys. Tested end to end against a scratch agentcloud session; the typed herdr adapter
   comes later, after agreement with chat-bridge on the shared lock.
4. **Tier-1 summaries**: an optional async worker using a configured command (default
   `claude -p --model haiku …`) with the fixed NOW/NEW/NEEDS contract and a cache. Measured before and
   after on real deltas.
5. **Trial**: run the watcher in print mode beside the existing 10-minute cron for a day, compare what
   each would have told the coordinator, and only then ask coord about switching.

## 7. Open questions and untested points

- **Q1.** Does Claude cross-session `SendMessage` into a `--dangerously-skip-permissions` coordinator
  deliver, or is it held for approval? Test with a scratch bypass session (it needs a human-approved
  launch, since the classifier refused one here), or with coord's consent on the real coordinator.
- **Q2.** Does herdr debounce `agent_status_changed` during the short gaps between tool calls? Measure
  on a scratch herdr pane.
- **Q3.** Does herdr expose pane *input* activity, so "the owner typed in the last 5 s" can be
  detected? If not, fall back to "composer non-empty" from `herdr agent read`.
- **Q4.** Codex transcript path resolution for a herdr pane (pid → rollout file).
- **Q5.** Latency and cost of a direct gateway API call for tier 1, compared with the 34 s `claude -p`.
- **Q6.** Where the watcher should run for agentcloud-hosted coordinators: devbig014 systemd, or an
  agentcloud `subscribe` cron (`agentcloudctl subscribe --spec '{"kind":{"kind":"cron",…},"deliver":{"target":"llm",…}}'`),
  which survives host loss (owner directive 2026-09-28: no task may assume the host is durable).

## Appendix: where things are

- ORC: `orc-engine/src/engine/engine_run.rs:54,57,404,495,539`; `orc-engine/src/engine/core.rs:12-19`;
  `orc-engine/src/effects/main_loop.rs:33,40-127`; `orc-engine/src/polling.rs:411-445,448,812-848`;
  `plugins/stdlib-core/workflows/work-assignment.ts:7-33`; `plugins/stdlib-core/workflows/routing.ts:27-35,93-105,125-136`;
  `plugins/10x-orc/config.ts:166-181`; `orc-engine/src/dispatch.rs:196`.
- agentctl (agent-utils 138dcfd5): `rs/agentctl/src/cli.rs:67-144`; `rs/agentctl/src/agent.rs:31,65-74`;
  `rs/agentctl/src/chat_events.rs:163-186`; `rs/agentctl/src/client.rs:27`; `rs/agentctl/src/chat_service.rs:44`.
- Scrollback cron: `temp/coord-scrollback-delta.py:4-17` (untracked, in the shared parent checkout).
- Claude binary checked: `claude_code_native` 2.1.285 (dotslash object `…claude-linux-x64/executable`,
  240,327,864 bytes); interactive tests ran 2.1.284 and 2.1.285.
