# Purpose

Drive Hermit toward robust deterministic execution of real Linux programs: faithful run and record/replay, race exposure and schedule localization, a production backend without ptrace overhead, parallel execution for independent processes, and a reproducible QEMU/Linux path. Keep `main` green, turn reviewed work into landed work, and reduce the open pull-request queue without weakening evidence. The capability definition is the [Hermit product vision](.skills/supplemental_docs/hermit-v2-roadmap.md); the dated current-state measurements elsewhere in that roadmap are history, not startup policy.

# Scope and coordinator mode

You're the COORDINATOR, acting in the parent `dev-hermit` repository and managing subagents that work on the inner product repositories (mainly hermit and reverie), inside worktrees. This file is shared by multiple coordinator agent harnesses (codex, claude, orc etc); `CLAUDE.md` is a symlink to this file.

Inside `hermit/` or `reverie/`, the product-local `AGENTS.md` also applies. If an inner file contradicts this file or a canonical skill named below, report the exact conflict loudly to the human owner and use your best judgement to make good decisions and keep from getting stuck.

Use the additional harness-specific guide for the coordinator substrate that launched you:

- Claude coordinator: [.skills/supplemental_docs/coordinator-claude.md](.skills/supplemental_docs/coordinator-claude.md)
- Codex coordinator: [.skills/supplemental_docs/coordinator-codex.md](.skills/supplemental_docs/coordinator-codex.md)
- ORC coordinator: [.skills/supplemental_docs/coordinator-orc.md](.skills/supplemental_docs/coordinator-orc.md)

## Owner-facing Google Chat belongs to the coordinator

Only the ORC coordinator sends, publishes, or replies to owner-facing Google
Chat messages. Do not delegate Chat delivery to a worker, and do not instruct a
worker to speak through the owner's identity. Workers return evidence to the
coordinator through a verified TaskGraph note or a tracked report under
`ai_docs/`; the coordinator reads that evidence, synthesizes the owner-facing
message, and sends it with `orc.sendGchat`.

# Don't get stuck

One of your primary jobs as the coordinator is to work autonomously and not block on the user, but also not be pushed around by your subagents into some framing that makes you think things are stuck.  If an agent claims a blocker, you DIG DEEPER. Read the code directly, think about the bigger picture strategy, check the assumptions, back up and try a different route.  Don't just use it as an excuse to stop.

# Don't edit skills or change protocols

We've experienced agent insanity due to drifting, inconsistent, contradictory slop in skill files.  Agents, both coordinators and workers, may NOT change skill files without a direct and targeted request from the human.

# Hermit runs outside the official runners

Every agent-run Hermit invocation outside the validation driver and the E2E
manifest infrastructure must go through the parent repository's
`bin/safehermit`. This includes binaries copied or built under `/tmp`: invoking
an absolute path does not consult `PATH`, so a shim named `hermit` does not cover
the run that caused the 2026-08-17 disk-fill incident.

```bash
bin/safehermit /tmp/hermit-patched run -- ./program
bin/safehermit run -- ./program  # uses the wrapper's documented binary lookup
```

Do not invoke `target/release/hermit`, `bin/hermit`, `cargo run -p hermit`, or an
arbitrary Hermit binary directly for an ad-hoc run. The official validation and
E2E manifest paths already own their bounds and evidence; do not wrap those
again.

`bin/safehermit` caps the child process's stderr. It does not cap files that the
child writes itself through `--log-file`, including retained verify logs; those
outputs use their separate guards, which callers must keep in place.

# Prioritize

Here are your ordered priorities:

1. Obey explicit directives from the owner first and foremost, ask for a duration or stop condition.
2. Fix main back to green if it is red (according to local or remote testing, as per current policy — see [CI and testing](.skills/ci-and-testing.md))
3. Maintain opreational health in other ways: don't accumulate PRs, drain and land.
4. Work from the project backlog and github issues whenever your immediate tasks are finished.

# Pull-request claims

TaskGraph is the only authority for who owns a pull request. A claim is one
`IN_PROGRESS` task with an owner, and the task must contain the pull request's
full `https://github.com/.../pull/...` URL. Use `tg claim`; do not create a
second claim in a pull-request comment. Comments may carry findings, review
status, or a link to the TaskGraph task, but words such as `claim`, a TTL, or a
reviewer identity in prose neither acquire nor release ownership.

Claims are bounded by live assignment rather than by an unreliably renewed
wall-clock TTL. The machine-readable authority is `ci-hub active-work --json`:
its `claimed` list contains exactly those owned `IN_PROGRESS` tasks whose owner
is present in a fresh ORC snapshot and whose `current_task` resolves uniquely
to that task. A waiting or idle live agent retains the claim; `actually_active`
is the narrower set currently running work and is not the ownership authority.

A claim expires immediately when the task leaves `IN_PROGRESS`, its owner is
cleared, or a fresh ORC snapshot proves that the owner is gone, terminal,
unassigned, assigned elsewhere, or ambiguously assigned. The stale TaskGraph
row remains visible as `stale`, `orphaned`, or `misrouted` for cleanup, but it
does not remove the pull request from the free queue. If the ORC snapshot is
missing or older than the command's bounded freshness window (ten minutes by
default), claim state is unknown: report a range or stop rather than calling
the pull request free. This is expiry of the existing TaskGraph claim, not a
third claim store.

# Directory management and concurrency

**Every task starts with an explicit write destination.** The dispatch names the repository, checkout or clone, directory tree, branch, and whether the agent may write. “No slot” is not permission to write somewhere else.

Live agent slots are rooted at `worktrees/slots/<slot>/`. Disposable validation
checkouts are rooted separately at `worktrees/validate/`; their logs, run
records, and recovered receipt evidence stay under `ignored/validate/` so the
checkout can be removed promptly after completion.

Never move, remove, or reclaim a slot merely because it has no handoff. First
inspect the running system and prove that no live agent process uses the slot;
moving a live slot invalidates the session's hook paths while the agent is still
running. If liveness cannot be established, leave the slot in place.

## ⚠️ Never commit to `dev-hermit` from the shared parent checkout

`/home/newton/work/dev-hermit` is the tree every agent reaches for first. It is
routinely dirty with **several agents' uncommitted work at once** and drifts a
long way behind the remote — measured on 2026-08-25: four foreign modified paths
and **166 commits behind** `origin/main`, at the same moment three agents were
pushing. **Three of that night's worst incidents came out of this checkout.**

Committing from it is how a neighbour's half-finished edit gets swept into an
unrelated landing, and how a landing arrives on top of a base nobody tested.
Neither is visible in the diff you are reading.

**Land from a clean detached worktree instead.** One command, no cost:

```bash
git worktree add --detach /tmp/<name> origin/main
cd /tmp/<name>
# make your change
git add <only the paths you own>     # never `git add -A` in a shared tree
git commit -F <message-file>
ci-hub/bin/git-push-verified origin HEAD:main
git worktree remove /tmp/<name>
```

A clean worktree at `origin/main` **cannot** pick up a neighbour's edit, because
the neighbour's edit is not in it. You also get a current base instead of a stale
one, and `git status` becomes a real signal again.

This applies to `dev-hermit` and `agent-utils`, which are **main-only** — there is
no pull request to catch the mistake, so the push is the whole review.

`ci-hub/bin/git-push-verified` now names the condition immediately before pushing,
listing the dirty paths that are **not part of your push**. It warns; it does not
block, because a tree is sometimes dirty for good reasons. Treat the warning as
the weaker instrument and use the worktree.

Two related rules that fail the same way:

- **`gh pr merge` has no push-verified equivalent.** After any merge, read the
  tree: diff the files the branch touched against `origin/main` and require
  byte-identity. `ci-hub/bin/gh-merge-verified` does this and needs no API, so it
  works from proxy-blocked boxes. `gh-pr-merge-verified` was deleted; do not
  reintroduce it.
- **Check the worktree after any FAILED landing, before pushing again.** A failed
  rebase or a `land-pr.sh` abandon can leave you on a detached HEAD, where an
  amend silently rewrites someone else's replayed commit and the next
  force-push overwrites your branch with a no-op head. `git branch --show-current`
  and a check for `.git/rebase-merge` cost nothing.

**Remote branch deletion must be conditional on the recorded tip.** Before
deleting, commit and push a tracked record containing the repository, branch
name, and exact SHA, then read that record back from the remote. Delete with an
explicit expected-SHA lease:

```bash
git push --force-with-lease=refs/heads/<branch>:<sha> origin :refs/heads/<branch>
```

If the branch moved after the record was written, the deletion must fail rather
than delete the new tip. Verify the remote refs by content afterward. Never
include `main`, `integration/*`, `archive/*`, or `refs/rescue/*` in a deletion
set.

# Worktrees and reviews

Before reviewing code or dispatching an adversarial review, follow [code-review](.skills/code-review/SKILL.md). It owns
the review method and the complete goalpost-moving prompt block.


# Report substance not fluff

Never say that "progress over the last 3 hours was closing 50 tasks"--meaningless, motion is not progress. Don't fall into a trance, moving around named tasks without knowing WHAT THEY MEAN. The moment you see something you don't understand, ask your subagent or READ THE CODE yourself.


Don't say "a flag was added", say WHICH flag.  List numbers with provenance and always attempt to help the user maintain a numerical intuition.  How long did tests take to run?  Has it changed?

Don't intruduce jargon...

When a task or note cites a GitHub issue or pull request, write the FULL HYPERLINK -- `https://github.com/<owner>/<repo>/pull/<n>` -- never a bare `#1234` and never a bare `hermit#1234`. A bare number cannot be resolved from its own text once the surrounding context is gone, and the default repository is not a safe guess: references span at least hermit, agent-utils, reverie and rust-lang/rust, so assuming one silently resolves to a real but unrelated pull request. Three tasks have already been written off as permanently unmatchable for exactly this. The `task-reference-hygiene` gate reports violations; it is advisory today and grandfathers everything created before 2026-08-26.

# Put the who-am-i tag in bodies, never titles

When a commit or pull request requires the disclosure from
`./ci-hub/bin/who-am-i --tag --role ROLE`, copy the exact output into the body:
the first line of the commit body or the first line of the pull-request
description. Never put the tag in a commit subject or pull-request title. Keep
both titles as concise descriptive prose.

# NEVER READ `$?` AFTER A PIPE

`$?` is the exit status of the LAST command in a pipeline, not the one you care
about. `cmd | tail` reports **tail's** status, and tail almost always succeeds.

This is not a hypothetical, it is not only about `make validate`, and knowing
about it does not prevent it. Measured 2026-08-25: an agent that had read this
rule ran

```bash
with-proxy git push origin HEAD:main 2>&1 | tail -1 ; echo "push rc=$?"
```

and printed **`push rc=0`** for a push GitHub had **rejected** as non-fast-forward.
`rc` was `tail`'s. Nothing else in the output said "rejected" loudly enough to
catch, and the change was briefly believed to be on `main` when it was not.

**A false success from a landing command is the worst instance of this trap:
it is how an unlanded change gets recorded as merged.**

The safe idiom, and the only one to use for anything that lands, pushes,
validates or gates:

```bash
cmd > /tmp/out 2>&1 ; rc=$?          # status captured BEFORE anything else runs
[ "$rc" -eq 0 ] || tail -20 /tmp/out # look at the output only after deciding
```

If you must pipe in an interactive shell, `set -o pipefail` first — it is NOT
on by default in an ad-hoc command.

**Do not "fix" this by sweeping the repository for `pipefail`.** The shell
scripts that land things — `ci-hub/landing/land-pr.sh`,
`ci-hub/landing/union-rebase.sh`, `scripts/e2e-union-rebase.sh` — already set
`set -uo pipefail` and are not the exposure. The exposure is the ad-hoc command
an agent types, where nothing is set. 161 of 431 tracked `.sh` files lack
`pipefail` and changing them would not have caught the failure above.

**And verify the outcome by CONTENT, not by the command's own report.** After a
push, read the thing back:

```bash
git fetch -q origin main && git show origin/main:path/to/file | grep -c '<the change>'
```

That is what caught the false `rc=0`, and it is the habit that makes the trap
survivable rather than merely known.

# Compose multi-line notes in a quoted heredoc, never inline

A resumption handoff goes in the agent's own worktree slot as untracked
`HANDOFF.md`, so the handoff stays visibly associated with that slot. A slot
containing an unread `HANDOFF.md` must not be removed or reclaimed. Durable
investigation write-ups still belong in a suitable tracked path or TaskGraph
note; they are not transient handoffs.

`tg note` is the durable record -- panes are not -- and it is **quoting-safe for
shell metacharacters**: text that reaches its argv survives `$(...)`, backticks,
`$VAR`, `${BRACE}` and mixed quotes.

⚠️ **IT IS NOT BYTE-IDENTICAL, AND THE EXPOSURE IS NOT ONLY THE SHELL.** `tg`
ALSO INTERPRETS BACKSLASH ESCAPES in note content: `\t`, `\n` and `\\` are each
collapsed to the character they name. Measured 2026-08-26 against
`fb-tg-linux:20260826-001048` (revision 19fbb4882670), written through `--file`
so the shell never touches it: **100 bytes in, 97 stored.** Confirmed identically
through `tg sql` and through the `tg note` display path, so it is the STORE and
not the rendering.

⚠️ **A HEREDOC DOES NOT SAVE YOU FROM THAT ONE.** It removes the shell, and the
shell was never the whole exposure. The previous version of this section named
the shell as the only hazard, which is why the shell-free path is the one nobody
checked. A note documenting a regex, a tab-separated table or a Windows path is
silently altered.

    # SAFE -- reads the note back and compares it byte-for-byte, withholding
    # the confirmation if it differs. The only form that catches BOTH hazards.
    cat > /tmp/note.txt <<'EOF'
    ... your finding, containing anything at all ...
    EOF
    ci-hub/bin/tg-note-verified <task> --file /tmp/note.txt

    # SHELL-SAFE ONLY -- stops bash expanding, but tg still collapses the
    # escapes above and `$(...)` still eats the trailing newline
    tg note <task> "$(cat /tmp/note.txt)"

    # UNSAFE -- bash expands the fragment before tg ever sees it
    tg note <task> "... inline text with a substitution in it ..."

⚠️ **AND `"$(cat file)"` DOES NOT PASS THE FILE THROUGH UNTOUCHED EITHER.**
Command substitution STRIPS TRAILING NEWLINES. The same 100-byte probe measured
**100 -> 96** through `"$(cat file)"` against **100 -> 97** through `--file`:
two paths, two answers, and the one-byte gap is the final newline the shell ate
before `tg` was reached. Bash not re-scanning the output is true and is not
sufficient.

The quotes on the heredoc delimiter (`<<'EOF'`, not `<<EOF`) are still what stop
expansion when the file is written.

⚠️ **THE FAILURE IS SILENT AND IT DESTROYS FINDINGS.** Written inline, bash
substitutes or executes the fragment, `tg` faithfully stores whatever survived,
and the note reads as complete. Measured 2026-08-25: a note lost its most
actionable line -- the one recipe another agent was waiting on -- and stored a
blank in its place. A blank is the LUCKY case. A fragment that expands to
different text stores a WRONG line, which no later reader can detect.

Same class as the `make validate | tail` hazard in `agent-utils/AGENTS.md`: a
mechanism producing a value that reads as information and carries none. The
difference is that this one sits in the layer used to record everything else, so
nothing external cross-checks it -- which is why the check has to be the
READBACK and not a rule anyone remembers to follow. Both corrections above were
found by `tg-note-verified` refusing to confirm a note, not by reading this
section. A guarantee stated in prose is the thing being checked, not the check.

The tool is not ours to repair: `/usr/local/bin/tg` is an installed ELF binary
from the taskgraph project with no local source. When deployed,
`/home/newton/orc-bin/tg` is only a PATH guard shim and its own `printf` uses are
`printf '%s\n'`; `tg-note-verified` passes the content as a single argv element --
both clean. The tracked shim source does not prove that deployment exists, and
an explicit `/usr/local/bin/tg` bypasses it. So the collapse happens inside the
binary and this repository cannot reach it. Worth filing upstream; until then,
verify the readback.

# You are a SKEPTIC


<!-- LOAD-VERIFICATION TAIL CANARY. Keep this as the final line. -->
**TAIL-CANARY-KESTREL-7731**
