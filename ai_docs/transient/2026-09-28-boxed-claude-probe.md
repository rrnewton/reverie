[hermit2, coord-boxtest-claude, claude-opus-5-5, devbig014, role=impl]

# Boxed Claude probe, slot coord-boxtest-1 (2026-09-28)

Harness: Claude Code, model claude-opus-5-5, on devbig014. Every Bash probe
ran from `/home/newton/work/dev-hermit/worktrees/slots/coord-boxtest-1`, with
the exit status captured before anything else ran
(`cmd > /tmp/o 2>&1; rc=$?`). Each file a probe created was removed afterward.

## 1. Location

| Command | rc | Key output |
|---|---|---|
| `pwd` | 0 | `/home/newton/work/dev-hermit/worktrees/slots/coord-boxtest-1` |
| `id -un` | 0 | `newton` |
| `cat /proc/self/cgroup` | 0 | `0::/user.slice/user-212630.slice/user@212630.service/3pai_sandbox.slice/run-p1873881-i270290075.scope` |
| `findmnt -no OPTIONS /tmp` | 0 | Two stacked mounts: btrfs `subvol=/tmpvol` (rw), with tmpfs on top: `rw,nosuid,nodev,relatime,size=16777216k,uid=212630,gid=100` |
| `echo $HOME $TMPDIR` | 0 | `/home/newton` (TMPDIR is **unset**) |

## 2. Write probes (`: > PATH`)

| Path | Result | Error text |
|---|---|---|
| slot `PROBE` | WROTE | |
| `~/work/dev-hermit/ai_docs/transient/PROBE` | WROTE | |
| `~/work/dev-hermit/experiments/PROBE` | WROTE | |
| `~/work/dev-hermit/PROBE` | DENIED rc=1 | `Read-only file system` |
| `~/work/PROBE` | DENIED rc=1 | `Read-only file system` |
| `~/agent_scratch/PROBE` | DENIED rc=1 | `Read-only file system` |
| `~/PROBE` | WROTE | (private per-slot layer: `$HOME` is backed by `dotsync-home/work/dev-hermit/worktrees/slot-state/agent/coord-boxtest-1/home`) |
| `/tmp/PROBE` | WROTE | |

Extra probes beyond the requested list:

| Path | Result | Note |
|---|---|---|
| `~/.cache/PROBE` | WROTE | per-slot, as documented |
| `~/.claude/PROBE` | WROTE | `~/.claude` is a stacked ro+rw mount, and the rw layer wins |
| `~/.claude/projects/-home-newton-work-dev-hermit/memory/PROBE` | WROTE | the **shared** auto-memory directory is writable |
| `~/work/dev-hermit/.git/PROBE-coord-boxtest-claude` | WROTE | the primary checkout's `.git` is rw (needed for worktree commits) |
| another slot's `target/ci/PROBE-coord-boxtest-claude` (hermit-kvm-process-waits-01a0d5c2) | DENIED | `Read-only file system` |

## 3. Claude's own file tools

| Tool | Target | Reported |
|---|---|---|
| Write | `/tmp/claude-tool-probe.txt` | `File created successfully` |
| Edit | same file, `line one` -> `line one edited` | `updated successfully`; `cat` confirmed the new content |
| rm (Bash) | same file | rc=0; `ls` confirms it is gone |
| Write | `~/work/dev-hermit/PROBE-claude` | **failed** as expected: `EROFS: read-only file system, open '/home/newton/work/dev-hermit/PROBE-claude.tmp.1873881.5fd4a7630725'`; no file was left behind |

## 4. Git in the slot

| Command | rc | Key output |
|---|---|---|
| `git status --short \| head` (pipefail) | 0 | (empty: tree clean) |
| `git log --oneline -1` | 0 | `bbae3fba85 Pin hermit 10770b2f so the scorecard tool and the root SCORECARD.md agree again` |
| `git branch --show-current` | 0 | `coord/boxtest-1-20260928` |

## 5. Identity and tooling

| Command | rc | Key output |
|---|---|---|
| `ci-hub/bin/who-am-i --tag` | 0 (!) | `[hermit2, unknown-agentname, opus, devbig014, role=impl]`, plus the stderr line `DEGRADED: identity resolver returned UNRESOLVED` |
| `herdr agent list \| head -c 300` (pipefail) | 0 | JSON listing works; the first entry is `coord` (claude, idle, cwd `/home/newton/work/dev-hermit`) |
| `gh api user -q .login` | 1 | `gh wrapper: could not resolve the stored rrnewton-bots token for github.com. Repair that login explicitly with /usr/bin/gh; refusing to fall back to another account.` |
| `/usr/bin/gh auth status` | 1 | `You are not logged into any GitHub hosts.` |

## What broke

1. **No GitHub access at all.** Inside the box, `~/.config/gh/hosts.yml` is a
   char device `1,3` owned by root, which is `/dev/null` bind-mounted over the
   token file. Both the `~/bin/gh` wrapper and `/usr/bin/gh` see no login. The
   brief says "commit and push from here as usual", but `gh` cannot work. I did
   not attempt a `git push`, so whether git-over-https still authenticates by
   some other route is untested.
2. **The identity resolver cannot name the agent.** `DG_AGENT_NAME` is unset in
   the box. `--diagnostics` shows that every slot-based signal fails because it
   reads the legacy `worktree-state.json` ("has no producer ... read the
   wrkslots registry at worktrees/ACTIVE.<machine>.json instead"). The tag
   comes out as `unknown-agentname` with **exit 0**, so a script that copies the
   tag into a commit body gets a wrong tag silently. The model field also says
   `opus`, not `claude-opus-5-5`. The first line of this report was written by
   hand from the dispatch.
3. **TMPDIR is unset**, and the `/tmp` tmpfs is mounted `nosuid,nodev`. Hermit
   uses TMPDIR, falling back to `/tmp`, so this probably matters for any Hermit
   run inside the box (see memory `tmpdir-on-a-nosuid-or-nodev-filesystem-breaks-every-hermit-run`).
   I ran no Hermit binary, so this is not verified.
4. **Box gaps (writable when a stricter box might deny them):** `~/.claude` is
   rw, including the auto-memory directory shared with every other Claude
   session, and `~/work/dev-hermit/.git` is rw. The `.git` exposure is probably
   necessary because the slot's worktree metadata lives there. It also means the
   box does not stop a write to the primary checkout's refs or index.
5. **No harness failures from the box itself.** The session transcript contains
   exactly two errors, and both are the intentional probes (the EROFS Write on
   `~/work/dev-hermit/PROBE-claude` and the Bash probe loop). Session start,
   hooks, the transcript `.jsonl`, and the `~/.claude.json` update (in the
   private `$HOME` layer) all succeeded. `findmnt` does show five stale
   `~/.claude.json.tmp.*` paths bind-mounted read-only. These look like
   leftovers of atomic-rename temp files from earlier sessions that the box
   froze into its mount table; they did no harm here.
