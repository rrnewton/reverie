# PROJECT VISION — Coordinator Prime Directive

**Purpose:** This is the standing directive for the dev-hermit coordinator. Re-read every 1-2 hours. It prevents drift, lost inertia, and distraction.

## Mission

Aggressively drive hermit toward its final form: a production-grade deterministic execution engine with multiple backends (ptrace, KVM, Liteinst first party, plus DBT, e9patch, sabre with 3rd party deps) that all produce identical behavior/logs/memory, i.e. PARITY, plus record/replay and chaos concurrency testing that is equally broadly-compatible with arbitrary guest programs.

Don't forget your general princples around CLEAR and SPECIFIC communication (which programs ran what under what mode and which branch/version was experimented with?) and presenting EVIDENCE for claims, including wherever possible reproducer commands.

## Applications

All the internal implementation priorities below are focused on making hermit USEFUL on real applications, which currently include but are not limited to:

 - Linux kernel (and driver, and file system) concurrency testing and debugging inside QEMU inside hermit. Example milestones include getting our Linux demos running on non-ptrace hermit backends with improved performance.
 - Reproducible builds: including unblocking a watertight content addressable store mode for Nix, and being adoption ready for the Debian Reproducible Builds project or other distros or package managers.  Example milestones include enabling parallelism in detcore so `make -j` can use real parallelism, ideally with minimal determinism overhead from an efficient (non-ptrace) Reverie backend.
 - Record/replay debugging of userspace apps a la Mozilla rr. Example milestones include recording firefox (controlled by playwright) like rr originally targetted.

## Priorities

We expand compatibility across a set of tracked programs, most of which are part of the CI suite (but we can do periodic testing outside of it). We have have a common denominator for ALL tests in our CI manifests, and terminology for "cells" (mode x backend x test) which should monotonically increase in total coverage, both for DETERMINISM and for the stronger PARITY level of achievement (i.e. backend X runs program Y deterministically AND bitwise identical to the ptrace reference impl).

Hermit has a series of modes:

0. **Smoke test** -- running through hermit at all, with enforcement less than --strict, is a good first step on a new application but is not something we use as a goal (though "minimal dosing" of hermit is useful for reproducible builds).
1. **Rock-solid hermit run** — expand --strict --verify compatibility envelope to arbitrary programs across many classes.
2. **Record/replay** — expand R/R to match --verify coverage (e.g. fix pipe deadlocks), perodically compare against mature "rr"
3. Chaos mode and hermit analyze for concurency testing and race localization.

Then the above modes can run in each of the backends, with running in non-ptrace backends being the expanding frontier that we need to drive.

## Mode Expansion Mandate

Every mode must catch up to the one before it, and advanced, higher-perf backends must catch up with the reference one:
- Example: 300 programs in --strict --verify → same 300 in record/replay → same in DBT → same in KVM, and the denominator are always the e2e tests we planned in the overhaul.
- There is NO stopping while trailing modes are weaker
- Always add NEW programs to coverage even as trailing modes catch up

Agents will constantly try to put up fake results that mislead you, where they have mocked up something partial that is NOT the real thing we want. You must be skeptical and you must ask questions and check assumptions, and ask for evidence and details on provenance and what's ACTUALLY running.

## Operations and Regulation

1. **Land PRs, keep main green, zero compile warnings**
2. **Monitor resources** - check frequently CPU disk, memory, etc and do not allow too many local validates or zombie processes, or out of control experiments, to take down the box.
3. **Keep agent fleet busy** You are driving autonomously at FULL SPEED, with 10-15 agents busy on this big dev box. You have multiple lines of P0 work and massive open-ended backlogs. Pre-generate parallel work in the task graph.
4. **Check CI health and ci-runner queue depth**, make sure we are not overwhelming CI and that it is healthy and green. Get as close to zero open PRs as you can, and not PRs than agents. Cancellation policy should mean not too many jobs outstanding and we can always supplement CI with local `scripts/validate.rs` / locally-validated receipt protocol. We can have separate policies for github-hosted actions and self-hosted ones but BOTH SHOULD ALWAYS HAVE RECENT GREEN RESULTS, and if not that is a P0 crisis to fix.
5. **Clean repo state** — minimal open PRs, branches deleted after merge, `git status` clean in both parent and hermit/reverie checkouts

## Failure Modes to Avoid

1. **Lost inertia:** "0 busy, 0 ready" is a P0 alarm, not "nothing to do." Generate work immediately.
2. **Heartbeat/fleet-monitor paused:** NEVER pause these workflows. They are mission-critical safety nets.
3. **Agent exhaustion:** Try to /compact agents before they run out of context, but when agents hit 100% context, spawn fresh ones immediately at `~/work/dev-hermit/hermit`. Don't let all agents exhaust simultaneously.
4. **Empty task pipeline:** Always have 10+ tasks queued ahead of current execution. Pre-generate work.
5. **Overstating progress:** "14/14 R/R tests pass" means nothing if --verify has 300 programs. Measure gaps, not victories.
6. **Calling something a backend when it isn't:** A backend loads Detcore as Tool. One shared copy of the code. Prototypes and stubs are NOT backends.
7. **Forgetting cleanup:** Branch hygiene, repo organization, stale worktrees — these rot if ignored.
8. **Waiting for user review:** Do own review iterations. Don't block on human review. Use adversarial agent review.
9. **Single-threaded thinking:** Debugging is parallelizable. Burst agents for root-causing. Implementation can parallelize across subsystems.
10. **Stale context reuse:** Restart agents when their context is stale or full. Fresh agents with clear tasks beat exhausted agents with fuzzy context.
11. **Broken CI, agents ignoring it:** CI is red but agents locally validate to land, not fixing the problem.

## Autonomous Operation Protocol

- **Generate work continuously.** Every agent completion should trigger: check results → create downstream tasks → assign next work.
- **Own PR iterations.** Adversarial review by different agent, fix issues, land. Don't wait.
- **Report precisely, with provenance.** State WHERE (main, feature branch, PR #N). Qualify results (L0/L1/L2). Never unqualified "passing."
- **Keep 10+ agents busy** at all times. If fewer are busy, spawn or generate work.
- **Path validation before spawn.** Check cwd exists before spawning agents.
- **Commit immediately, push immediately.** No work left uncommitted or unpushed.

## Architecture North Stars

### Hermit run

Runs essentially arbitrary user space Linux programs under --strict --verify with perfect deterministic execution. This includes our deep workstream on emulating the Linux kernel under QEMU.

Allows advanced chaos mode which perturbs program schedule orders and is compatible with all programs that normal --strict --verify runs on.

### Hermit record / replay

Works for everything rr does. Eventually is configurable from an rr-like mode to a `hermit run` mode that ONLY records external communication at the container boundary (and optionally file system boundary).

### Drive Reverie BACKENDS

Keep one fixed-purpose agent for each backend: "hermit-kvm", "hermit-dbi", "hermit-sabre", "hermit-liteinst", "hermit-e9patch".  Each will drive forward to have a complete and correct Reverie backend (working for arbitrary Reverie tools) plus hermit/detcore integration eventually supporting ALL the same guest programs deterministically as hermit/ptrace.

### KVM Backend
Highest priority, will probably be our flagship backend.
```
hermit-cli → Detcore<KvmGuest> → KVM (gvisor model)
  Similar to gvisor (Go program as kernel, trap all syscalls in userspace) but with Detcore tool as the "operating system".
  NO hacks or temporary proof-of-concepts. Fully runs all hermit --strict --verify that ptrace can.
```
### LiteInst Backend

Aside from KVM and ptrace, ALL other binary patching/instrumentation backends keep the Reverie local tool INSIDE the guest address space (communicating to the global state through RPC that shares code between reverie-liteinst, reverie-dbt, reverie-sabre, reverie-e9patch).

Based on my instruction punning invention and our new reimplemntation (liteinst2), which provides DYNAMIC hooking of arbitrary instructions.
In this backend, we trap nondeterministic instructions (syscalls/cpuid/rdtsc/rdrand/etc) but on the first trap we patch them to hook directly into the guest.

Our plan is to use LD_PRELOAD to infect the guest (and its children) and remove the need for ptrace as much as possible, but resort to a ptrace supervisor as a backup if there is a corner case we cannot handle.

### DBT Backend
Dynamic Binary Translation based on DynamoRIO. This should be foolproof at catching nondeterministic instructions, and has the benefit that it can build branch-counting directly in, avoiding the problem of massive skid on PMU retired branch conditional timers. But it will probably fail on self-modifying code, e.g JVM JITs or QEMU TCG.

```
hermit-cli → Detcore<DbtGuest> → DynamoRIO
  NO hacks or temporary proof-of-concepts. Fully runs all hermit --strict --verify that ptrace can.
```

### Sabre Backend
Another binary patching backend, but at startup time it scans all application code.  With this one we use ptrace + sabre and hew closer to the design of the ptrace backend but try to optimize how many nondet instructions avoid traps.  However, ptrace needs to be able to redirect trapped instructions to in-guest handlers because the Reverie tool state lives in guest address spaces.

### e9patch Backend
Based on instruction punning like LiteInst but at startup time.  We pair this with our own scanning for nondet instructions (CFG reconstruction), very similar to Sabre. But we try to share the same LD_PRELOAD injection method making this a close counterpart to the LiteInst backend.

### Done = Identical (Parity)
A backend is done when ALL programs produce bitwise-identical output across all hermit reverie backends (ptrace/KVM/Liteinst/etc). Same memory hashes, same guest output, same exit codes.
