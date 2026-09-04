# Provenance

Evidence from the independent exact-head review of
`https://github.com/rrnewton/hermit/pull/2950` ("Preserve CPUID subleaf selection
in Detcore"), carried out on 2026-09-04 by agent `review-cpuid` under task
`exact-head-review-of-the-two-cpuid-subleaf-pull-requests`.

`subleaf_probe.c` is the guest behind the finding that withheld approval from
that pull request. It is recorded here because the finding is a statement about
what three backends return for the same CPUID request, and that statement is
only checkable if the guest that produced it still exists.

The same review approved `https://github.com/rrnewton/reverie/pull/521` at
`034797c85dc617905e249f94b089bde00388605f`. That approval rests on reverie's own
in-VM test rather than on this probe, so nothing here is evidence for it.

## Revisions

| role | SHA |
|---|---|
| hermit base, `origin/main` throughout the review | `4d8f866102882b6eeabdf99cc7e81433cc3c95c5` |
| hermit PR 2950 head, the reviewed head | `c3a85da26110fd8dfc31433c8d4109726127703b` |
| reverie pin in hermit at both of the above | `320412c5967790939ebe405c73e394ffd9c41459` |
| reverie PR 521 head, approved separately | `034797c85dc617905e249f94b089bde00388605f` |

`4d8f8661` is the parent of `c3a85da2`; the branch was not behind main at any
point during the review. The reverie pin is identical on both sides, so every
difference between the base and head legs below is the hermit diff and nothing
else.

## Published verdicts

- Changes requested at `c3a85da2`:
  https://github.com/rrnewton/hermit/pull/2950#issuecomment-5545346256
- Approved at `034797c8`:
  https://github.com/rrnewton/reverie/pull/521#issuecomment-5545344917

## Host

`devbig014`. `hermit host-capabilities` reported `cpuid-faulting: PRESENT`
(`arch_prctl(ARCH_SET_CPUID, 0) = 0`, `/proc/cpuinfo` advertises `cpuid_fault`)
and `kvm: PRESENT`. The host CPU is AMD; `logs/native-host-cpu.log` shows what it
returns with no hermit involved, which is why the base and head legs are the only
comparison that means anything.

## Working locations

Every hermit measurement ran in worktree slot
`worktrees/slots/review-cpuid`, a checkout of `rrnewton/hermit` at `c3a85da2`.
None ran in the shared parent checkout at `/home/newton/work/dev-hermit/hermit`,
whose local `main` was frozen at `78aabb5b6d26` — the pre-fix base, where a
measurement would reproduce the old behaviour and read as the fix not working.

Every hermit invocation went through the parent repository's `bin/safehermit`,
as the parent `CLAUDE.md` requires for agent-run hermit outside the validation
driver and the E2E manifest infrastructure. The `test-harness` cell runs are the
manifest infrastructure and own their own bounds.
