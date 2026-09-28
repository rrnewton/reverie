# Boxed-agent experiment on a fresh test slot (2026-09-28)

Owner request (Chat `spaces/AAQAWBA0g6A/messages/scPU3tsvorQ.scPU3tsvorQ`): read
the owner's slots-overhaul handoff (`SLOTS_OVERHAUL.md`) and try the latest
agentctl features for boxing subagents into worktree slots, but do not move the
FAT lanes yet. This is the handoff's Step 2 experiment, adapted to a NEW slot
(`coord-boxtest-1`) instead of relaunching a working lane. Nothing was rolled
out. `.wrkslots.yml` was not edited, and every sandbox setting was passed on the
command line. Worker: `coord-subagent` (Claude, devbig014), dispatched by the
claude coordinator in pane `wJ:p1`.

## Setup

| Item | Value |
|---|---|
| agent-utils with the box | `bada63e3054798ba22b17ca2b5018ca9f37f7119`, as a shared read-only worktree at `/var/tmp/coord-slots/au` |
| agentctl | built from that tree: `cargo build --release -p agentctl`, 21.4 s |
| Tool worktree | `/var/tmp/coord-slots/dh`, a dev-hermit worktree whose `hermit/agent-utils` is bada63e. Its `ci-hub/bin/wrkslots` was used for `run` and `shell-command` only |
| Slot | `coord-boxtest-1`, branch `coord/boxtest-1-20260928`, created and held with the primary checkout's own launcher (agent-utils `d81ae950`). Create took 94 s wall, including a configuration-lock wait and the recursive submodule init. Size: 2.1 GB after a release build, of which the hermit `target/` is 1.0 GB |
| Isolation | `--isolation root` for every run. Extras, from the handoff: `--read-write '/var/facebook/credentials/$USER/agent_x509' --home-shared .rustup` |
| Host | devbig014: 316 cores. Load averages sampled during the session: 33.71/60.67/70.49, and 38.16/50.84/55.57 at the end |

## Write table

The handoff's Step 2.4 probe loop, run headless with
`wrkslots run coord-boxtest-1 --isolation root ...`:

| Path | Result |
|---|---|
| the slot | WROTE |
| `~/work/dev-hermit/ai_docs/transient/` | WROTE |
| `~/work/dev-hermit/experiments/` | WROTE |
| `~/PROBE` | WROTE (lands in `worktrees/slot-state/agent/coord-boxtest-1/home/`; the real `~/PROBE` never existed) |
| `/tmp` | WROTE (private tmpfs, `rw,nosuid,nodev`, 16 GB) |
| `~/work/dev-hermit/PROBE` | DENIED: `Read-only file system` |
| `~/work/PROBE` | DENIED: `Read-only file system` |
| `~/agent_scratch/PROBE` | DENIED: `Read-only file system` |
| `~/work/dev-hermit/worktrees/PROBE` (the registry) | DENIED |

The before/after check from outside the box (listings of `~`, `~/work`,
`~/agent_scratch`, the primary checkout's top level, its `git status --porcelain`,
`ai_docs/transient`, and `experiments`, taken at 20:37:37Z and 20:37:58Z) showed
one new entry, `worktrees/slot-state/`, the slot's private HOME layer. That is
the expected result.

A wider probe found more:

- Writable: the slot's `hermit/` and `reverie/`, `.git/worktrees/coord-boxtest-1/`,
  the private `~/.cache`, and, for all launches, `~/.claude`, `~/.codex`,
  `~/.cargo/registry`, `/run/user/212630`, and `/dev/shm`.
- The primary checkout's whole `.git` is writable, including `refs/heads`,
  `hooks`, and **all 92 of 92** `.git/worktrees/*` admin directories. A boxed
  agent can therefore move any branch or rewrite another slot's HEAD or index,
  and it can install a hook that later runs outside the box.
- Read-only: other slots, `worktrees/slots`, the primary's `hermit/`, `reverie/`,
  `ci-hub/`, `ignored/`, `.git/modules/hermit`, `~/.config`, `~/.cargo/bin`,
  `~/bin`, `~/.local/bin`, `/var/tmp`, `worktrees/slot-state`, and
  `worktrees/slot-images`.
- Hidden: `~/.ssh` (0 entries), `~/.git-credentials`, `~/.config/gh/hosts.yml`,
  `~/.netrc`, and `~/.arcrc`, which all read as empty.
- `sudo -n` works inside, as the handoff says.

Headless runs land in
`3pai_sandbox-wrkslots-coord\x2dboxtest\x2d1.slice/wrkslots-run-...scope`
(TasksMax=8192).

## Workflow probes

| Probe | Outcome | Exact error | Workaround |
|---|---|---|---|
| 5a `git commit` | **works** (2.2 s, mostly dev-hermit's hooks) | Two refusals, both repo policy rather than the box: the shared-index guard rejects a bare commit, and commit-attribution rejects `role=worker` (`canonical disclosure role must be one of coordinator,debugger,human,impl,relay,reviewer; got 'worker'`) | Commit with a pathspec, and use a valid role (`impl`) |
| 5a `git push --dry-run` | **fails** with the default setup | `gh wrapper: could not resolve the stored rrnewton-bots token for github.com.` then `fatal: could not read Username for 'https://github.com'` | Export `GH_TOKEN` (read with `/usr/bin/gh auth token --user rrnewton-bots`) before `wrkslots run`; the box forwards it. Then **append** `credential.https://github.com.helper` entries (an empty reset, then `!/usr/bin/gh auth git-credential`) after the ambient `GIT_CONFIG_*` list. Push dry-run: rc=0 in 1.3 s, to `https://github.com/rrnewton/dev-hermit.git`, pre-push health hook included |
| 5a `FLEET_REAL_GH=/home/newton/bin/gh with-proxy gh api user -q .login` | **fails**, even with `GH_TOKEN` forwarded | same `gh wrapper: could not resolve ...` | `with-proxy /usr/bin/gh api user -q .login` prints `rrnewton-bots` when `GH_TOKEN` is forwarded. The `~/bin/gh` wrapper strips `GH_TOKEN` and reads only the hidden `hosts.yml`, so it cannot work in the box |
| 5b `cargo build --release --locked -p hermit` in the slot | **works**: 97.3 s wall, 221 crates, `target/` 1002 MB, `CARGO_BUILD_JOBS=48` | none | A no-op rebuild without `--home-shared .rustup` also works (0.2 s), because the pinned nightly was already installed. `.rustup` needs to be writable only when a toolchain must be installed |
| 5b `bin/safehermit <slot>/hermit/target/release/hermit run -- /bin/echo hi` | **works** from the SLOT's `bin/safehermit` (rc=0, 0.35 s), with `TMPDIR=/tmp` and with `TMPDIR` inside the slot. The PRIMARY's `bin/safehermit` **fails** | `mkdir: cannot create directory '/home/newton/work/dev-hermit/ignored/safehermit/20260928T204724Z-273446': Read-only file system` | Use the slot's own copy; it logs to `<slot>/ignored/safehermit/` |
| 5b the guest is **not boxed** | see "What broke", item 1 | a guest wrote `/var/tmp/coord-slots/escape-probe-guest`, a path the box denies; guest cgroup `app.slice/safehermit-20260928T204827Z-491301.service` | none inside the box |
| 5c rust-script, cold per-slot `~/.cache` | **works**; one-time compile cost | none | Times, cold then warm: `ci-hub/ledger/check-ledger-events.rs` 4.7 s then 0.05 s; `ci-hub/health/agent-container-reconcile-bridge.rs --help` 2.9 s then 0.014 s; `ci-hub/ci-hub.rs --help` 48.0 s then 0.11 s. The same `ci-hub.rs` run outside the box, from the same slot path, also took 42.4 s cold, because the rust-script cache is keyed by path. A fresh slot pays this either way; a boxed relaunch into an existing slot pays it once more |
| 5d `herdr agent list` | **works** (0.011 s) | none | It returns every pane, the FAT lanes included, because `/run/user` stays writable. The box does not limit what a boxed agent can do to other panes through herdr |
| 5d `ci-hub/bin/wrkslots status` | **fails** inside the box | `REFUSED: cannot open state lock /home/newton/work/dev-hermit/worktrees/ACTIVE.global.lock: [Errno 30] Read-only file system` (rc=3) | Run it outside. Even a read-only `status` needs the lock file. The handoff says the same about heartbeat, finish, and write-handoff. Note that `wrkslots status` takes no slot argument |
| 5e a worktree nested inside the slot | **works** | none | dev-hermit `git worktree add --detach <slot>/sub/wt1 origin/main` 2.8 s, commit 1.1 s. hermit `git -C hermit worktree add --detach <slot>/sub/hwt1 HEAD` 0.28 s, then `submodule update --init agent-utils` 0.96 s (served from the local mirror by the global `insteadOf`), commit 0.03 s. A worktree in the private `/tmp` also works. All three were removed with `git worktree remove --force` inside the box, their admin dirs are gone, and the admin-dir counts are unchanged (dev-hermit 96, hermit 0) |

## Boxed Claude through agentctl

1. **Profiles.** Profiles resolve from `<--cwd>/.agentctl/profiles.json`, and that
   file is untracked. `agentctl profiles --cwd <slot>` printed `"profiles": []`,
   while the primary checkout has six, including `claude-opus-55` (harness claude,
   model opus, no extra argv, no env). I copied the primary's file byte for byte
   into `<slot>/.agentctl/` (`cmp` identical, modes 700/600), and selected
   `claude-opus-55` unchanged. `.agentctl/` is ignored through the shared
   `.git/info/exclude`, so the slot's `git status` stays clean. agentctl's
   registry was pointed at `<slot>/.agentctl` with `--registry`, so nothing was
   written to the primary's `.agentctl/`.
2. **First launch fails.** The command was
   `AGENTCTL_WRKSLOTS_BIN=/var/tmp/coord-slots/dh/ci-hub/bin/wrkslots agentctl start coord-boxtest-claude --cwd <slot> --registry <slot>/.agentctl --profile claude-opus-55 --slot coord-boxtest-1 --slot-isolation root --slot-project ~/work/dev-hermit --file brief.md`.
   It exited 75 with `the slot relay program in pane wJ:p4R exited during startup`.
   The message says the tab was retained, but the pane was already gone.
   Running the same boxed command in a pty showed the cause:
   `Error setting permissions on staging directory '/var/facebook/credentials/newton/agent_x509/mntspace/claude_code_newton': EROFS: Read-only file system`.
   `agentctl --slot` runs `wrkslots shell-command`, which passes only
   `--isolation` to `run`. `--read-write` and `--home-shared` can therefore come
   only from `.wrkslots.yml` `sandbox:`, which this experiment could not edit.
   **Until that configuration exists, a boxed Meta Claude cannot start through
   agentctl at all.**
3. **Workaround used.** I pointed `AGENTCTL_WRKSLOTS_BIN` at a shim,
   `/var/tmp/coord-slots/wrkslots-inject.sh`. For `shell-command` only, it inserts
   `--read-write '/var/facebook/credentials/$USER/agent_x509' --home-shared .rustup`
   into the printed `run` line. With the shim, `start` returned rc=0 in 26.5 s.
4. **No trust prompt.** The slot's private `~/.claude.json` is seeded from the
   real one, which already records `hasTrustDialogAccepted: true` for
   `/home/newton/work/dev-hermit`, a parent of every slot. The Down/Enter
   procedure was not needed.
5. **The run.** Claude followed the brief, including the "## Where you can write"
   block. It wrote `ai_docs/transient/2026-09-28-boxed-claude-probe.md` in 1 min
   31 s, and removed every probe file it created (checked from outside).
   `agentctl wait` returned idle, and `agentctl stop` returned rc=0 in 1.4 s,
   closing the pane and the tab. The slot's slice was empty afterwards. Claude's
   report is committed next to this file.
6. **Findings from inside Claude** (details in its report):
   - The Meta launcher moves the harness to
     `3pai_sandbox.slice/run-p1873881-i270290075.scope`, which is not the slot's
     wrkslots slice. The file-system view still applies, since writes were
     refused, but the slot's slice limits do not cover the harness.
   - `TMPDIR` is unset in the pane.
   - `gh` has no login.
   - `who-am-i` prints `[hermit2, unknown-agentname, opus, ...]` with exit 0.
     The unknown name is not specific to the box: my own `who-am-i` outside the
     box printed `[hermit2, unknown-agentname, claude-opus-5-5, devbig014, role=impl]`
     with `DEGRADED: identity resolver returned UNRESOLVED`. Inside the box the
     model field is the launch alias `opus`, not the model ID.
   - The shared Claude auto-memory directory is writable.

## What broke, ranked

1. **`bin/safehermit` runs Hermit outside the box.** It starts the run as a
   `systemd-run --user` service (not `--scope`), so the guest runs in the user
   manager's `app.slice`. That is outside the box's mount namespace and outside
   the slot's slice. A guest wrote to `/var/tmp/coord-slots/`, which the box
   denies. This is the handoff's known limitation ("`systemd-run --user` without
   `--scope` starts work outside the box"), but dev-hermit **requires** every
   ad-hoc Hermit run to go through `bin/safehermit`, so no boxed agent's Hermit
   run is ever boxed. The same thing also explains why the box's `nosuid,nodev`
   `/tmp` did not break Hermit (memory
   `tmpdir-on-a-nosuid-or-nodev-filesystem-breaks-every-hermit-run`): the guest
   sees the host `/tmp`, not the box's. A guest that writes to `/tmp/x` writes
   to the host, and the agent cannot see that file.
2. **agentctl cannot pass sandbox extras.** Without `read_write: [agent_x509]` in
   `.wrkslots.yml`, a boxed Claude dies at startup (item 2 above). The handoff's
   Step 1 configuration is a prerequisite, not an option. Step 1 also must not
   land before Step 0: every lane runs wrkslots `d81ae950`, which rejects YAML and
   unknown keys (the separate compatibility audit covers the details).
3. **"Commit and push from here as usual" is false today.** Hiding
   `~/.config/gh/hosts.yml` breaks `gh` and git-over-https. The working route
   (forward `GH_TOKEN`, override the credential helper) needs the launcher's
   environment, and agentctl refuses `--env` together with `--profile`. For
   boxed agentctl agents, pushing and `gh` therefore need an owner decision.
   Options: un-hide `hosts.yml` for this project; forward a token through the
   configured `sandbox.env`; or have the coordinator push from outside.
4. **The per-slot `~/.cache` costs 4.7 GB on the first boxed Claude launch.**
   `~/.cache/dotslash` filled with 4.4 GB during the first launch; the host copy
   is 22 GB and already had it. Another 340 MB went to rust-script. Sharing
   `.cache/dotslash`, a content-addressed store, would remove most of that. That
   is an owner configuration choice.
5. **The primary `.git` is fully writable** (refs, hooks, and all 92 admin dirs).
   This is necessary for slot commits under the current layout, but it is the
   largest gap in the accident boundary.
6. **Traps worth a line in the brief:**
   - The harness environment already carries `GIT_CONFIG_COUNT=3` (rewrites of
     `git@github.com:`, `ssh://`, and `git://` to https). A script that sets
     `GIT_CONFIG_COUNT=2` silently drops them, and the push falls back to ssh:
     `ssh: connect to host github.com port 22: Network is unreachable`.
   - The primary's `bin/safehermit` cannot log from inside the box.

## Recommendations

**(i) Boxed-agent brief and launch recipe.** Do this before any real boxed launch:

- land Step 0, then Step 1, with at least `read_write: [/var/facebook/credentials/$USER/agent_x509]`;
- decide on GitHub authentication (item 3) and on sharing `.cache/dotslash`;
- decide whether `bin/safehermit` should use `systemd-run --scope`, so that a
  boxed agent's Hermit runs stay in the box and the slot's slice. That is a
  policy question, and it needs an issue.

Launch recipe:

- copy `.agentctl/profiles.json` into the slot, and select the profile exactly;
- pass `--registry <slot>/.agentctl`, or a coordinator-owned registry;
- `AGENTCTL_WRKSLOTS_BIN=<tool root>/ci-hub/bin/wrkslots`;
- `--slot <slot> --slot-isolation root --slot-project ~/work/dev-hermit`.

Add these lines to the handoff's brief block:

- use the slot's own `bin/safehermit`;
- `wrkslots` registry commands, `status` included, fail inside the box; ask the
  coordinator;
- never overwrite `GIT_CONFIG_COUNT`, only append to it;
- use a valid disclosure role (`impl`, not `worker`);
- until item 3 is decided, pushes and `gh` do not work, so hand the commit SHA to
  the coordinator.

**(ii) FAT lanes that need several checkouts.** `agentctl --slot` takes exactly
one slot. Worktrees nested inside that one slot work (5e), including a hermit
worktree with an initialized submodule, so a FAT lane can live in one boxed slot
if it follows these rules:

- name nested worktrees uniquely. A nested dev-hermit worktree's admin dir lands
  in the primary's shared `.git/worktrees/<basename>` (here
  `.git/worktrees/wt1`). Every `git worktree list` shows it, and wrkslots's
  unregistered-worktree check will see it;
- keep them under an ignored path inside the slot. Otherwise the slot shows
  `?? sub/`;
- `git worktree remove` them before exit;
- never place one in the box's `/tmp`, which vanishes at exit and leaves a stale
  admin entry in the shared `.git`.

The alternative, one box spanning two slots through `--read-write <other slot>`,
was not tested. It would need the shim or configuration, because agentctl cannot
pass it.

## State left behind

- Slot `coord-boxtest-1` is HELD and in place, and its tree is clean on
  `coord/boxtest-1-20260928`. The scratch branch `coord-boxtest-scratch` was
  deleted, and it was never pushed (dry runs only).
- The slot's `.agentctl/` holds the copied profiles and two archived agent records.
- The private HOME layer (4.7 GB) is at
  `worktrees/slot-state/agent/coord-boxtest-1/home`.
- `/var/tmp/coord-slots/{au,dh}`, the probe scripts, and the shim remain.
- Raw logs are under `/tmp/coord-boxexp/`. They are not durable; this file is the
  record.
