# Correction to the first commit of this directory

The first commit of this directory, `00364314abb43e6fb7121f63496c9f149fbf9596`,
carried a **wrong** `logs/head-c3a85da2-ptrace.log`. It was labelled as the head
leg and contains base behaviour. `logs/head-c3a85da2-kvm.log` from that commit
was produced by the same wrong binary; its values are right, because the KVM
backend does not consult Detcore's table at all, but the label was unearned.

Caught on readback from the remote, before anyone else read it. Corrected here.

## What happened

The slot's source tree was clean at `c3a85da2` and `git status --short` said so.
The **binary** was not: `target/debug/hermit` was still the build made from the
base-reverted sources three commands earlier, because two attempted restores had
run without a following `cargo build`.

The first of those restores also failed silently for a second reason worth
naming: `git checkout <sha> -- <paths>` **stages** those paths, so the
`git checkout -- <paths>` that followed restored them from the index — that is,
restored the base content over itself. `git checkout HEAD -- <paths>` is the
form that undoes it. `git status` then reported a clean tree, which was true and
useless: a clean tree says nothing about which binary is on disk.

This is precisely the trap `RERUN.md` in this directory warns about. Writing the
warning did not prevent falling into it, which is the whole argument for the
guard below rather than for a more careful reader.

## Where the truth was, all along

`bin/safehermit` records the sha256 of the binary that actually ran, under
`ignored/safehermit/registry/`. Reading it back settled the question in one step
and distinguished the two possibilities that matter — a stale binary, which is a
process error, versus the same binary producing different answers, which would
have been a far more serious finding about hermit itself:

| time (UTC) | recorded binary | leg |
|---|---|---|
| `19:07:32` | `05c7db82a852…` | head ptrace and head KVM — the original correct measurements |
| `19:10:00` | `458329500c4b…` | base ptrace — correct |
| `19:19:21`–`19:19:38` | `458329500c4b…` | the three re-captures that produced the wrong artifact |
| `19:24:30`+ | `05c7db82a852…` | re-verification: the head binary is stable at three runs |

`05c7db82a852…` is the head build, `458329500c4b…` the base-reverted build. They
are different binaries, so this was a stale binary and not nondeterminism.

## What did not change

Nothing about either verdict, and nothing in either published review comment.
Both rest on the `19:07:32` and `19:10:00` measurements, which were taken with
the right binaries and agree line for line with the corrected logs here. The
manifest-cell and unit-test logs were never affected: those legs rebuild inside
the command that runs them.

## The guard now in place

Every log in `logs/` produced by a hermit run carries a
`# hermit binary sha256:` header naming the binary that produced it, and
`RERUN.md` records the capture procedure that refuses rather than continues when
the base and head binaries hash the same or the restored binary does not hash
back to the head build. Those headers can be checked against
`ignored/safehermit/registry/` on the machine that ran them, which is an
independent record this directory does not control.
