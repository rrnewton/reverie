# https://github.com/rrnewton/reverie/pull/653 compared with reverie main: what is still unique

**Revisions**

- **M**: reverie main, 7a5053329079c03137156678b5ec72f94ac85978.
  - It is `origin/main` in the read-only worktree /home/newton/work/dev-hermit/worktrees/slots/cml-653r3 (confirmed with `git rev-parse origin/main`).
- **P**: the head of https://github.com/rrnewton/reverie/pull/653, fce2b4847c64611fe10c7fbd7e4b11d985b28a61 (7 commits).
- **B**: the pull request's base, b0ede531e00dd1e068d0e0b1f220edddcf96d4b5.
  - Main has 14 commits since B.
  - Nine are about reparenting: cefb5473d, e04a92cf2, 0e5984d10, 6ffd92632, 84fe70f2f, 2e2912d10, a5298631e, 92a6da155 and cb1eb7bf2.
  - Five are about the order of exit and end-of-file: 9da4c830e, 79459027d, 057c69a6b, e8bc88068 and 7a5053329.
- **H**: Hermit 18c8c426.
  - The Hermit worktree /home/newton/work/dev-hermit/worktrees/slots/cml-hermit-3233 is at a9cf3263, which does not descend from 18c8c426.
  - `git diff --stat 18c8c426 a9cf3263` is empty for the three cited Hermit files: detcore/src/syscalls/threads.rs, detcore/src/tool_global.rs and hermit-cli/tests/cli.rs. The citations therefore hold at both revisions.

**Conventions**

- Reverie paths are under `reverie-kvm/src/` unless they begin with `tests/`.
- `psp` means `process_signal_publication.rs`.
- "M executor.rs:4227" means line 4227 of that file at revision M.

**Method**

- I only read code. I built nothing, ran nothing and posted nothing.
- I marked claims I did not establish by reading as **(inference)**.
- Kernel, glibc and bash behaviour is **(from memory)** of their sources.

## Conclusion

**653 is superseded as a unit. It still carries five specific fixes to the wait path that should be re-landed on main. Main is not unsafe for Hermit.**

### 1. Do not rebase or merge 653

- **Main replaced 653's reparenting core on its own.**
  - Nine of the 14 commits since B do this, and both sides rewrote the same exit transition in psp.
- **For Hermit's process shape, the two sides behave the same.**
  - In that shape, the root is pid 3 and its init, pid 1, is outside the guest.
  - Both sides reparent running orphans to pid 1 and reap zombies without the guest seeing them. Main does this at M psp:199-236, called from 596-601. 653 does it at P psp:688-698, 743-745 and 833.
- **Main has three things for that shape that 653 lacks.**
  - **An asynchronous root join**, at M runtime.rs:3256 and 3258, from commit a5298631e.
    - 653 keeps B's blocking join: B runtime.rs:3210/3212 is the same code as P runtime.rs:3224/3226, which reaches P executor.rs:3384 and then 3432.
    - A root that exited with a live child or orphan hung on that join. At pin B, Hermit's `root-exits` mode ended with rc 124 after 91209 ms, according to the body of https://github.com/rrnewton/hermit/pull/3244.
    - 653's diff does not touch those lines, so a rebase would keep main's join.
    - However, all of 653's evidence was gathered with the blocking join, so none of it transfers.
  - **The parent pid in /proc, rendered when the file is read** (M executor.rs:2258-2289).
  - **Descriptors closed before the terminal receipt**: 9da4c830e and the four commits after it.
- **653's one distinctive capability does not apply to Hermit.**
  - That capability is adopting orphans into a guest root that is pid 1, with waits shared across the process.
  - It applies only when the traced root is itself the init of its PID namespace.
  - Hermit never has that shape. Main deliberately refuses it and fails the run (M psp:165-168 and 538-541).

### 2. Re-land the wait-path fixes as one new, small pull request on main

653 fixed these in code that main left untouched. The four wait functions are byte-identical between B and M, and main has none of the ten mechanisms 653 added (§4.3).

- **R1:** `WaitSelector`. It handles process-group selectors, returns ESRCH for a pid of INT_MIN, and validates waitid ids.
- **R2:** validate the arguments before synchronizing, and let a raw waitid accept a null infop.
- **R3:** reap the child before copying out the results, and return EFAULT after the reap.
- **R4:** take the child atomically under the family lock.
- **R5:** cancel a blocked wait. Fix the three round-7 defects instead of carrying them over.

### 3. Main is not unsafe for Hermit

Main has six defects in this area (§2):

- **D1: High, but only standalone.** A blocked wait is never woken.
  - Under Hermit it is masked, because detcore never lets the guest block inside the backend.
- **D2a: Medium, reachable under Hermit.** A blocking wait4(0) or wait4(-pgid) returns ECHILD when a signal wakes it with no child ready.
  - The result is deterministic, but the errno is wrong.
- **The other four are Low.**
- **None of the six is on the path of the Hermit bash test.**

### 4. The Hermit bash test is expected to pass on main as pinned by https://github.com/rrnewton/hermit/pull/3244

- The test is `run_kvm_bash_process_substitution_is_deterministic`, at H hermit-cli/tests/cli.rs:3119-3146.
- This is a static inference; I did not run it. The reasoning is in §5.6.
- The test passes vacuously when /dev/kvm, bash, paste or diff is missing (cli.rs:3121-3127). A PASS therefore needs its log read.

## One-screen summary

Classes: **(a)** equivalent, **(b)** different, **(c)** missing on main.

| Behaviour | Main compared with 653 | Guest-visible? | Reachable on main under Hermit? |
|---|---|---|---|
| Orphans in Hermit's shape (root pid 3, init pid 1 outside the guest) | (a) same parent pid; main adds the async join and /proc read-time rendering | | yes; main is better |
| Orphans when the root is init (pid 1) | (c) 653 adopts them; main refuses and fails the run | yes | no |
| wait4 with pid > 0 or -1; waitid with P_PID > 0 or P_ALL; WNOHANG; WNOWAIT | (a) | | |
| wait4(0) or wait4(-pgid) | (b) D2a and D2b | yes | D2a: yes, through detcore's poll |
| wait4(INT_MIN); waitid with P_PID ≤ 0 or P_PGID < 0 | (b) wrong errno, or reaps a child | yes | no; detcore answers first |
| Unsupported wait options | (b) D5: EINVAL arrives late, only once a child completes | yes | yes, through the legacy retry; the number of probes depends on host timing |
| waitid with a null infop | (b) D3 | yes | no; Hermit rejects it itself |
| EFAULT when writing results | (b) D4: EFAULT without reaping | yes | only with a bad guest pointer |
| Blocked wait during exit_group or a peer failure | (c) D1: hangs | yes | masked |
| A reap racing its own process's exit | (c) D6: spurious `FamilyWaitLedgerMismatch` abort | yes | presumably serialized by detcore (inference) |
| Waiter registration, and re-checking before blocking | (c) | only through D1 | masked |

**The six defects on main**

- D1 is High standalone. D2a is Medium. D2b, D3, D4, D5 and D6 are Low.
- D1 through D5 already existed at B, so main did not regress them. 653 fixed them, but the fixes never reached main.
- D6 is widened by main's reparenting to the outside init.

**653's round 7 was not clean**

- Codex requested changes, with three findings:
  - High: a leader that is not init is not woken when a peer fails.
  - Medium: argument errors bypass cancellation.
  - Medium: the end-to-end tests are ordered by spin timing.
- Claude approved with four Low findings.
- All three Codex findings fall on R5, and the re-land must fix them.

## 1. Behaviour inventory

| # | Behaviour | Class | Main (M) | 653 (P) | Guest-visible, and Hermit |
|---|---|---|---|---|---|
| 1 | Running orphan when a process that is not init exits, with init outside the guest | (a) | `install_namespace_reaper` installs `Outside{root.ppid}` when ppid is not 0 (psp:351-355). Root and Child exits then call `reparent_children_to_outside_init` (596-601, which runs 199-236). The parent pid in /proc is rendered at read time (executor.rs:2258-2289). | `reparent_orphans` runs at every exit (psp:833), and the new parent pid is published as `CONTAINER_INIT_PID` (psp:688-698). The /proc content is generated at open. | Same getppid result on both. A /proc file opened before the reparent and read after it is stale in 653; Linux generates the content at read time (from memory). Reachable under Hermit. |
| 2 | Unreaped zombie child of a process that exits, with init outside the guest | (a) | The WaitableZombie edge is dropped during the reparent (psp:218). | "With init outside the guest the zombie is reaped invisibly" (psp:743-745). The refusal applies only when there is a guest reaper (746-758). | Reachable under Hermit. |
| 3 | Orphans when the root is init (pid 1), a standalone shape | (c) | `DescendantReparentingUnsupported` for a reaper that is `Unknown` or `TracedRoot` (psp:538-541; rationale at 165-168). The run fails closed. | Adopts running orphans into the root (`guest_reaper`, psp:356). Refuses only an unreaped zombie, with `ZombieAdoptionUnsupported` (psp:746-758). | Linux adopts both. Not reachable under Hermit. |
| 4 | Host teardown when the root exits with live descendants | (b) | Asynchronous join (runtime.rs:3256/3258). | Blocking join (runtime.rs:3224/3226, then executor.rs:3384, then 3432), unchanged from B (runtime.rs:3210/3212). | Visible as a hang. Reachable under Hermit: at B this mode ended with rc 124 after 91209 ms. That 653's head hangs the same way is inferred from the a5298631e commit message; I did not run it. |
| 5 | Adopted children that any thread of the adopting process can wait for | (c), not needed on main | None. With init outside the guest, no guest process can wait for an orphan, which matches Linux. Main refuses the init-root shape. | `take_adopted` (psp:1099) and `observe_adopted_children` (executor.rs:3580). | Not reachable under Hermit. |
| 6 | One waiter registration per task | (c) | Nothing is registered. A blocked wait sits in its executor's own std mpsc `recv` (executor.rs:4268-4277 and 4362-4371). | `register_before_blocking` (executor.rs:3596-3621) calls `WaitCancellation::register` (terminal_read.rs:474-499). | Visible only through D1. |
| 7 | Re-checking before blocking | local children (a); cancellation (c) | Both sides rescan `pending_processes` before each `recv`, and the channel buffers completions (executor.rs:4233-4267). Main has no cancellation to re-check. | Same rescan (executor.rs:3752-3812), plus `check_child_wait_cancelled` (3569-3577) and an adoption rescan after registering (3596-3621). | Visible only through D1. |
| 8 | wait4 with pid > 0 and with -1 | (a) | Matcher at executor.rs:4227. The free emulator takes the smallest collected pid (17928). | `WaitSelector::Pid` and `WaitSelector::Any` (executor.rs:17161-17205). | Linux takes the first waitable child in fork order (from memory), which is the same unless pids wrap. 653's order differs from Linux only for adopted children (Claude round 7, Low 2). |
| 9 | wait4(0) and wait4(-pgid) | (b) | The synchronizing matcher never matches a group selector, so it returns without collecting (4227, 4240-4242). The free emulator then takes any already-collected child (17928), or returns ECHILD (17930-17931). | `Group(caller_pgid)` and `Group(-pid)`, which filter both collected and running children (executor.rs:3722-3725 and 3752-3757). | D2a and D2b. |
| 10 | wait4(INT_MIN) | (b) | Treated as a group selector: it reaps any collected child or returns ECHILD (17921-17931). | ESRCH, after the options check (17161-17205). | Linux returns ESRCH after the options check (from memory). See §8 on how detcore negates this pid. |
| 11 | waitid P_PID | id > 0: (a); id ≤ 0: (b) | ECHILD for id ≤ 0: the synchronizing path (4307-4310), then the free emulator's lookup (17979-17981) and ECHILD (17985-17987). | EINVAL (17161-17205). | Linux returns EINVAL. Under Hermit, detcore decides first (H threads.rs:1842-1856). |
| 12 | waitid P_PGID | (b) | Any child, for any id (4311 and 17982). | Id 0 means the caller's group, id > 0 means that group, and id < 0 is EINVAL. | Under Hermit, detcore filters by group first. For a negative id it returns ECHILD rather than EINVAL, which is detcore's own divergence. |
| 13 | waitid P_ALL | (a) | | | |
| 14 | WNOHANG with valid options | (a) | Returns Some(0), or zeroes the siginfo and returns 0 (4259-4262 and 4346-4357). | Same, after checking for cancellation (3794-3798 and 3923-3937). | |
| 15 | WNOWAIT | (a) | Leaves the child in place and does not touch the ledger (18021-18024). | Consumes the child only without WNOWAIT (3863). | |
| 16 | WUNTRACED, WCONTINUED, `__WALL` and `__WCLONE` | (b) in when EINVAL arrives | Checked only in the free emulators (17918-17920 and 17974-17976), after synchronizing. This is D5. | Checked before synchronizing (3718-3721 and 3851-3854, then 17294-17302). | Both sides return EINVAL, although Linux accepts these options. |
| 17 | waitid with a null infop | (b) | EFAULT, before the options check (17971-17976). | Accepts it and checks the options first (17294-17302 and 17330-17336). | Linux accepts a null infop (from memory). Hermit rejects it itself (H threads.rs:1788-1794). |
| 18 | EFAULT when writing results | (b) | Writes first and returns EFAULT without removing the child (17934-17945 and 18007-18020). | Takes the child, then writes, on the synchronizing path (3731-3741 and 3866-3873). The free emulators still write first (17130-17153 and 17260-17287); that is dead code for a matching child (inference). | Linux reaps first (from memory). D4. |
| 19 | Blocked wait cancelled by exit_group | (c) | Nothing wakes it. This is D1. | `request_exit_group` calls `cancel`, which drains the blocked waits and wakes them after releasing the lock (terminal_read.rs:305 and 313-357). The waiting side is `block_for_child_completion` (executor.rs:3660-3706). | Visible as a hang. Masked under Hermit. |
| 20 | Blocked wait cancelled by a peer's failure | (c); incomplete in 653 | `record_worker_failure` only sets flags until the natural join starts (vm.rs:598-614). | Wakes only registered waiters in an init process (Codex round 7, High). | Visible as a hang. Masked under Hermit. |
| 21 | Atomic take against cancellation | (c) | None. This is D6. | Takes the registry lock (`unless_cancelled`, terminal_read.rs:460-468), then the family lock. The wait is cancelled if `terminal.contains_key` (psp:1094-1098). Otherwise the local take and `consume_waitable_zombie` happen together (1102-1107). | Visible as a spurious abort on main. |

## 2. Defects on main

D1 through D5 already existed at B. D6 is widened by main.

`synchronize_wait4`, `synchronize_waitid` and the free `wait4` and `waitid` are byte-identical between B and M. I checked this in this pass by hashing each function body:

| Function | Start line at B | Start line at M | Lines |
|---|---|---|---|
| `synchronize_wait4` | 3449 | 4221 | 59 |
| `synchronize_waitid` | 3524 | 4296 | 78 |
| free `wait4` | 16734 | 17912 | 37 |
| free `waitid` | 16789 | 17967 | 60 |

Each body has the same SHA-256 at B and at M.

### D1: a blocked wait is never woken by exit_group or by a peer's failure

**Rating:** High standalone; masked under Hermit.

**Scenario**

1. Leader L blocks in wait4(-1, 0) or waitid(P_ALL, WEXITED) for its fork child C.
2. C blocks reading a pipe whose write end L's process holds.
3. Worker W calls exit_group, or W fails.

**Linux**

- The process dies, so the write end closes.
- C reads end-of-file and exits.
- The run ends with the exit_group status.

**Main**

- `request_exit_group` does four things (vm.rs:648-672):
  - sets `cancelled`;
  - publishes the terminal-read cancellation;
  - sends SIGURG to the root and the workers;
  - notifies the cancellation channel.
- The SIGURG handler is empty (vm.rs:268-272), and nothing sends on the child-completion channel that L waits on (executor.rs:4268-4277 and 4362-4371).
- If std's mpsc `recv` retries after EINTR (inference), L stays blocked.
- L's process then never tears down, C never sees end-of-file, and the run hangs.
- A peer failure is handled the same way: it only sets flags until the natural join starts (vm.rs:598-614).

**653**

- Fixed for exit_group and for cancelling workers (P terminal_read.rs:313-357 and executor.rs:3660-3706).
- Not fixed for a leader that is not init when a peer fails (Codex round 7, High).

**Under Hermit: masked**

- Every wait that detcore injects falls into one of two cases:
  - it targets a child that has already physically exited (H threads.rs:1632 and 1687);
  - it carries WNOHANG. This covers the poll at 1671 and 1715, and the legacy path, which adds WNOHANG at helpers.rs:1107.
- The backend's `recv` is therefore never reached while a child is alive.
- I read all three injection sites, but not every caller of the backend (inference).

**Host timing:** standalone, whether L is already in `recv` when exit_group lands depends on host timing. Whenever it is, the run hangs.

### D2a: a blocking wait4(0) or wait4(-own pgid) returns ECHILD

**Rating:** Medium. The result is deterministic under Hermit, and Hermit can reach it.

**Main**

- The synchronizing matcher is `requested == -1 || requested > 0 && pid == requested` (executor.rs:4227).
- For 0 or a negative pid, nothing matches, so the function returns None without collecting any child (4240-4242).
- The free wait4 then takes `state.children.keys().next()` (17928). That set holds only children already collected.
- If no child has been collected, it returns ECHILD (17930-17931).

**Standalone:** wait4(0, 0) with a running child returns ECHILD at once instead of blocking. Whether a finished child has already been collected depends on host timing.

**Under Hermit**

- Detcore's managed path parks the thread in `wait_for_child_lifecycle` (H threads.rs:1674).
- The park can resume with no child ready. For example, a signal resumes it as `ResumeStatus::Signaled`.
- Detcore then injects `poll_call`: the original selector plus WNOHANG (H threads.rs:1671 and 1715).
- On main that poll is wait4(0, WNOHANG). It returns ECHILD whenever no child has been collected.
- Detcore then takes `Err(errno) => break Err` (1725).
- The guest sees ECHILD. Linux would return EINTR, restart the wait, or keep waiting.

**Open question:** I did not trace whether a signal whose disposition is to be ignored also resumes the park. If it does, more programs are exposed (inference).

**Not affected**

- Bash: a non-interactive bash waits with waitpid(-1, ...) (from memory).
- waitid with P_PGID: main treats it as "any child" and collects (executor.rs:4311).

**Fix:** R1 (P executor.rs:3722-3725 and 3752-3757).

### D2b: waiting on a foreign process group reaps any child

**Rating:** Low; standalone only.

- **Main**
  - wait4(-pgid), for a group that contains none of the caller's children, takes any collected child (executor.rs:17928).
  - waitid(P_PGID, id) does the same for any id (4311 and 17982).
- **Linux:** ECHILD, or EINVAL for a negative waitid id.
- **Scope:** there is only one process group, because setpgid and setsid return ENOSYS (P executor.rs:17207-17216 documents this). So only an explicitly foreign pgid triggers the defect.
- **Hermit:** detcore filters by group first and returns ECHILD when no child matches (H threads.rs:1597-1600, 1621-1627 and 1712-1714).

### D3: a raw waitid with a null infop returns EFAULT

**Rating:** Low; standalone only.

- **Main:** returns EFAULT at M executor.rs:17971-17973, before the options check at 17974-17976.
- **Linux:** checks the options first, then accepts a null infop and simply skips writing it (from memory).
- **Hermit:** rejects a null infop itself (H threads.rs:1788-1794).

### D4: EFAULT is returned without reaping

**Rating:** Low.

- **Main**
  - wait4 writes the status and rusage and returns EFAULT before `state.children.remove` (executor.rs:17934-17945).
  - waitid does the same (18007-18020, before 18021-18024).
- **Linux:** reaps first, then fails the copy-out. `kernel_wait4` releases the task inside `do_wait` (from memory).
- **Effect:** a retry reaps the same child again. Only programs that pass a bad pointer can see this.
- **Hermit interaction**
  - Detcore does not consume its own record after an error other than ECHILD: 1645 and 1709 bypass the consume block at 1737-1751.
  - So under Hermit, main's behaviour happens to agree with detcore's bookkeeping.
  - After R3, the next wait takes detcore's stale-ECHILD path (1638-1644 and 1690-1708), which is untested (Claude round 7, Low 4).

### D5: unsupported options are rejected only after synchronizing

**Rating:** Low. Hermit can reach it, and the number of probes depends on host timing.

**Main's synchronizing functions ignore the options** (executor.rs:4221-4279 and 4296-4373).

For wait4 with WUNTRACED, WCONTINUED or `__W*`:

- with WNOHANG and a running child, it returns 0;
- without WNOHANG, it blocks in `recv`;
- once the child is collected, the free wait4 returns EINVAL (17918-17920) and leaves the child unreaped.

For waitid without WEXITED, with WNOHANG and a running child:

- it returns 0 with a zeroed siginfo (4346-4357) instead of EINVAL.

**Linux** accepts WUNTRACED, WCONTINUED and `__W*`. Both sides return EINVAL for them. Only the late EINVAL, which depends on child state, is specific to main.

**Under Hermit**

- Detcore sends these options to the legacy retry (H threads.rs:1586-1592 and 1735).
- That path adds WNOHANG (helpers.rs:1107) and retries while the result is 0 (1117-1119).
- Each probe increments `poll_attempt` and records a retry event (1502, 1520 and 1529).
- The guest gets EINVAL after N probes, where N depends on when the child's host thread finishes.
- I did not establish whether N changes the guest-visible logical time.

**Not on bash's path:** non-interactive bash passes WUNTRACED only when job control is on (from memory).

**653** validates first (P executor.rs:3718-3721 and 3851-3854), so EINVAL comes on the first probe.

### D6: a reap racing its own process's exit aborts the run with `FamilyWaitLedgerMismatch`

**Rating:** Low and latent. It happens only in ToolControlled mode.

**Scenario**

1. Thread T1 reaps child C through the free wait, which sets `consumed_child_wait` (M executor.rs:17945-17946).
2. Before T1's ledger commit (executor.rs:6458-6461, which calls `record_consumed_child_wait` at 5535-5542), one of two things happens:
   - peer T2 calls exit_group;
   - the process takes a fatal signal (`force_signal_exit`, 5901-5904).
3. Either one runs `take_exit`, which calls `record_process_exit` (5944 and 5952-5953).
4. The reparent to the outside init then removes every direct-child edge (psp:596-601, which runs 218).
5. T1's `consume_child_wait` finds no edge. Because the mode is controlled, the run aborts.

**Linux:** the wait either completes or is killed. The process exits with the exit_group status, and there is no error.

**History**

- B had this window only on the branch where the parent is terminal (B psp:398-420). M psp:513-537 keeps that branch.
- Main's reparent to the outside init widens the window to every Root and Child exit.
- A peer's failure does not trigger it, because `record_process_failure` does not reparent (M psp:366-404).

**Timing and reachability**

- Standalone in ToolControlled mode, it depends on host timing.
- Under Hermit it is presumably serialized by detcore's turns (inference).
- According to the Claude round-6 review, Hermit selects ToolControlled when `kvm_shared_dequeue_timers` is set. That review also did not establish whether the interleaving is excluded.
- It needs a multithreaded process. Bash is single-threaded.

**Fix:** R4 (P psp:1087-1109).

### 2.7 Shared defects: both sides have these, so they are no reason to prefer either

- **A thread cannot wait for a sibling thread's fork child.** It gets ECHILD because `pending_processes` and `state.children` belong to each executor. Linux allows the wait unless `__WNOTHREAD` is set.
- **A host-blocked wait has no EINTR or SA_RESTART semantics.** The Claude round-6 review, Info (a), notes this. On main it is worse, because nothing at all wakes the wait.
- **These wait options return EINVAL:** WUNTRACED, WCONTINUED, WSTOPPED, `__WALL`, `__WCLONE`, and P_PIDFD.
- **When init exits, the namespace is not killed** (Claude round 6, Info (e)).
- **`record_worker_failure` only records the failure until the natural join** (M vm.rs:598-614; Claude round 6, Info (d)).
- **setpgid and setsid return ENOSYS**, so there is a single process group.
- **The vfork parent is not suspended** (Claude round 6, Info (h)). That review cited P; I did not re-verify it at M.
- **`waitability_pending` is an untimed condition-variable wait** (Claude round 6, Info (c)). It is still present at M executor.rs:1914.
- **waitid writes the siginfo before the rusage.** Linux copies the rusage first (from memory).
- **The ordering of exec against sibling cancellation has an open human-review TODO** on all three revisions (B and P vm.rs:838, M vm.rs:854). I did not trace an exec racing an exit_group to a conclusion.

### 2.8 Residual problems in 653 that a re-land must not carry over

- **The blocking root join** (§1, row 4).
- **Free emulators that write before removing the child.** This is dead code for a matching child (inference).
- **One process group is assumed** (P executor.rs:17207-17217). That is acceptable while setpgid returns ENOSYS.
- **Failed and typed-fatal exits are counted as cancellation.** This is documented (P executor.rs:3622-3641).
- **/proc content is generated at open.**
- **Adopted children are chosen by smallest pid, which differs from Linux's list order** (Claude round 7, Low 2; P psp:267-270).
- **A leader that is not init is not woken on a peer's failure** (Codex round 7, High).
- **Argument errors bypass cancellation** (Codex round 7, Medium; P executor.rs:3717-3721 and 3850-3854).
- **The end-to-end blocked-wait tests use a spin to order the waiter and the exiter.**
  - The spin is 150 million iterations (Codex round 7, Medium; tests/static_elf.rs:17846, 17891 and 17941).
  - No end-to-end cell covers the round-7 fix (Claude round 7, Low 3).

## 3. Round-6 and round-7 findings, re-derived against main

**Reviews read, all in /tmp/claude-coord-notes/**

- codex-review-kvm-reparent6.md, at 06a38443: changes requested.
- claude-review-kvm-reparent6.md.
- codex-review-kvm-reparent7.md, at fce2b484: changes requested.
- claude-review-653-r7.md, at fce2b484: approved.

| Finding | Status in 653 | Status against main |
|---|---|---|
| Codex round 6, High: a wait selects and consumes a child before checking for a committed cancellation | Fixed by the atomic take (P psp:1087-1109). | Main has no cancellation. The closest hazards are D6 and D1. |
| Codex round 6, Medium: the tests are ordered by spin timing | Fixed at unit level by the B1 barrier; end to end it still spins. | Main has no such tests. A re-land needs a handshake. |
| Codex round 6, Medium: waits on a local child owned by a worker are not covered | Still not covered end to end (Claude round 7, Low 3). | No coverage on main either. A worker that forks, waits and is then hit by exit_group hangs on main (D1). |
| Claude round 6, Medium: the local test cannot fail on round 5 | Discriminating cells added. | Required for R5: a bounded test that fails on main. |
| Claude round 6, Low: the `TerminalReadCancelled` label | Fixed with `ChildWaitCancelled`. | R5 needs the typed disposition. |
| Claude round 6, Low: Failed and typed-fatal exits classified as cancellation | Documented (P executor.rs:3622-3641). | Not applicable. |
| Claude round 6, Low: the ledger window in ToolControlled mode | Closed, per Claude round 7. | Present on main as D6. |
| Claude round 6, Info (a) to (h) | | (a) to (e) and (h) are shared (§2.7). (f), `rearm_after_exec` not asserting that blocked waits are empty, and (g), a child reported after the commit, apply only to 653. (g) is permitted by Linux. |
| Codex round 7, High: a leader that is not init is not woken on a peer's failure | Open. | Broader on main: nothing wakes any blocked wait (D1). |
| Codex round 7, Medium: argument errors bypass a committed cancellation | Open. | Main has no cancellation to bypass. R2 moves argument checks to the front, so R2 and R5 together must check cancellation before returning an argument error. |
| Codex round 7, Medium: the end-to-end tests use a spin and no blocked-state handshake | Open. | A requirement for the re-land. |
| Codex round 7, goalpost-moving: an exemption was added for argument errors | Open. | Must be closed, not carried over (§7). |
| Claude round 7, Low 1: no test fails if the executor stops using the atomic take (P executor.rs:3516-3524) | Open. | Required for R4. |
| Claude round 7, Low 2: smallest-pid selection of adopted children | Open. | Negligible on main, which has only fork children; there the smallest pid is fork order unless pids wrap. |
| Claude round 7, Low 3: no end-to-end coverage of the round-7 fix | Open. | Required for R5. |
| Claude round 7, Low 4: no Hermit-level evidence; detcore never reparents; the stale-ECHILD path is untested | Open. | Applies to R3. A Hermit test with a faulting status pointer is required. |

## 4. Tests

### 4.1 Tests that can be ported from 653

All names were verified at P.

**psp unit tests**

- `group_waits_select_collected_fork_children_by_process_group` (psp:4285).
- `group_waits_select_running_fork_children_by_process_group` (4332).
- The fork halves of three tests:
  - `wait4_int_min_is_esrch_after_the_options_check` (4387);
  - `waitid_rejects_a_negative_group_and_a_nonpositive_pid` (4440);
  - `raw_waitid_accepts_a_null_infop_and_validates_options_first` (4494).

**Cancellation and take tests**

- The tests:
  - `child_waits_of_a_cancelled_thread_take_nothing_and_write_nothing` (psp:6579);
  - `child_waits_of_a_terminal_process_take_nothing_and_write_nothing` (6664);
  - `blocked_child_waits_are_cancelled_only_for_the_threads_they_apply_to` (6886);
  - `a_take_before_a_cancellation_returns_and_the_next_wait_is_cancelled` (7027);
  - `a_faulting_wait_output_is_efault_after_the_reap` (7080).
- When porting them:
  - drop the adopted-child dimension;
  - use a leader that is not init. Codex round 7 notes that the blocked-wait tests use pid 1 (P psp:6730), which is main's refused `TracedRoot` shape.

**terminal_read.rs tests**

- `exit_group_wakes_blocked_child_waits_after_publishing_their_cancellation` (1613).
- `worker_cancellation_wakes_only_worker_child_waits` (1642).
- `child_wait_take_runs_under_the_registry_lock_only_before_a_cancellation` (1672).

**tests/static_elf.rs**

- `local_child_wait_is_cancelled_by_peer_exit_group` (18080).
- Extend it with waits owned by a worker, both blocking and WNOHANG, under exit_group and under exec.
- Replace the spin with an explicit handshake from guest to harness.

### 4.2 Tests that cannot be ported

- The adopted-orphan unit tests.
- The `finish_forked_process_*` tests in vm.rs.
- The two adopted static_elf tests, with 32 cells between them. The local test has 4.

All of these assert the init-root adoption that main refuses.

### 4.3 Main's own tests

- From B to M, main adds 55 `#[test]` lines and removes none (my count).
- They cover:
  - reparenting to the outside init;
  - the race to claim a child's exit;
  - rendering the parent pid in /proc;
  - the order of descriptor closing.
- None covers group selectors, INT_MIN, waitid id validation, a null infop, EFAULT when writing results, or cancelling a blocked wait.
- Ten of 653's identifiers have zero matches at M under reverie-kvm: `WaitSelector`, `WaitCancellation`, `take_collectable_child`, `block_for_child_completion`, `register_before_blocking`, `check_child_wait_cancelled`, `ChildWaitCancelled`, `waitid_argument_error`, `write_wait4_outputs` and `consume_waitable_zombie`.
- terminal_read.rs has an empty diff from B to M.

### 4.4 Numbers, with where each comes from

**My counts**

- From B to P, 653 adds 43 `#[test]` lines and removes 8.

**Claude's round-7 review at fce2b484** (claude-review-653-r7.md)

- 1325 tests passed, rc=0, in 29.47 s in total.
- Library tests: 904 at the base and 936 at the head. In two runs each, the base took 8.34 s and 9.74 s, and the head took 8.07 s and 8.65 s.
- static_elf: 362 tests, in 7.22 s and 7.86 s.
- 901 library tests are common to base and head.

**653's pull-request body** (a snapshot of earlier heads)

- Library tests: 910 passed in 40 s, run serially.
- static_elf: 359 passed in 5 s.
- Full local validation of 558edc60: VALIDATED, with 2/2 nodes and 7/7 gates in 632 s. That result is superseded and was never published.

**Hermit at pin B**

- Log: /home/newton/work/dev-hermit/ignored/validate/validate-kvm-hermit-dc7da377fed3-01a0d5c2-1790503136600853784-3849341e.log.
- The bash test failed in 0.570 s with exit 125: "KVM process SignalProcessId { tgid: Pid(6), generation: 4 } exited with child SignalProcessId { tgid: Pid(7), generation: 5 } still requiring unsupported reparenting".
- There is no validation log for af5bdb06.

## 5. Re-land plan

Land R1, R2 and R3 first. They are small and independent, and D2a, the only Medium that Hermit can reach, is fixed by R1. Land R4 and R5 second, together.

### R1: WaitSelector

- **Source:** P executor.rs:17161-17205.
- **Where it is used:** both synchronizing paths (P 3722-3725 and 3856-3859), and the free emulators.
- **Fixes:** D2a and D2b, ESRCH for INT_MIN, and waitid id validation.
- **Tests:** the psp tests in §4.1.
- **Effect under Hermit:** only D2a.

### R2: check the arguments first

- **Source**
  - P executor.rs:3718-3721: wait4 returns Ok(None), and the free emulator returns EINVAL.
  - P 3851-3854, with `waitid_argument_error` (P 17294-17302).
  - A null infop is accepted (P 17330-17336).
- **Fixes:** D3 and D5, and it removes D5's timing-dependent probe count under Hermit.
- **Constraint:** once R5 lands, the argument check must run after the check for a committed cancellation (Codex round 7, Medium).

### R3: reap, then copy out

- **Source:** P executor.rs:3731-3741 and 3866-3873.
- **Fixes:** D4.
- **Constraint:** it needs Hermit evidence: a guest with a faulting status pointer, run under `--strict --verify --verify-strict` (Claude round 7, Low 4). The reason is that detcore's stale-ECHILD path then handles the next wait (H threads.rs:1638-1644 and 1690-1708).

### R4: atomic family take

- **Source:** P psp:1087-1116 and P executor.rs:3502-3553.
- **Dependency:** it needs the typed `ChildWaitCancelled` disposition, so it lands with R5.
- **Adaptation:** fit it to main's reparent to the outside init, so that the take consumes the edge before `reparent_children_to_outside_init` can drop it.
- **Fixes:** D6.
- **Test:** it needs a test that fails if the executor splits the check from the take (Claude round 7, Low 1).

### R5: cancel blocked waits

- **Source:** P terminal_read.rs:305-357 and 421-499, and P executor.rs:3569-3706 without the adoption parts.
- **Fixes:** D1.
- **It must also:**
  - wake every blocked leader on a peer's failure, not only init (Codex round 7, High);
  - apply a committed cancellation before argument errors (Codex round 7, Medium);
  - cover waits owned by a worker end to end, under exit_group and under exec (Codex round 6, Medium; Claude round 7, Low 3);
  - use a handshake from guest to harness instead of the spin (Codex round 7, Medium).

### 5.6 Hermit's `run_kvm_bash_process_substitution_is_deterministic`

**Expected to pass on main as pinned by https://github.com/rrnewton/hermit/pull/3244.** This is a static inference.

**Why it should pass**

- **Hermit roots the guest at pid 3 with parent pid 1.**
  - 653's pull-request body says so, and says Codex confirmed it against Hermit 043f8c3b.
  - So main installs `Outside{1}` (M psp:351-355).
- **The pin-B failure no longer applies on main.**
  - That failure was pid 6 exiting while child 7 was still alive.
  - On main, that exit takes the Child branch and the reparent to the outside init (M psp:542-601).
  - The refusal `DescendantReparentingUnsupported` applies only to `Unknown` and `TracedRoot` (538-541).
- **None of main's defects is on the test's path.**
  - Bash waits with waitpid(-1, 0) or waitpid(-1, WNOHANG) (from memory), so D2a and D5 are not reached.
  - D6 needs a multithreaded process.
  - D1 is masked.
- **A root exit is handled.** If the root exits while a process-substitution child is alive, main's asynchronous join handles it; 653's blocking join would not.
- **The body of https://github.com/rrnewton/hermit/pull/3244 corroborates this.**
  - At pin B, `grandchild` failed closed in 1396 ms with rc 125, and `root-exits` hung, ending with rc 124 after 91209 ms.
  - With 7a505332, "all three complete". Their output is byte-identical between ptrace and KVM, and each backend matches itself under `--verify-strict`.
  - Its six-mode fixture covers both shapes.

**Not established**

- 3244 also carries three Hermit companion commits. They are not named in the part of the body I could read.
- Whether a bare pin bump without those commits passes.
- There is no log of a run at af5bdb06.

## 6. Where results depend on host timing

- **Standalone, both sides:** which collected child wait4(-1) returns depends on which children's host threads have been collected.
- **Main:** the order in which a child's exit is claimed depends on host timing when there is no embedder (M psp:646-651). That comment says detcore orders it; I did not verify this.
- **D1:** whether the waiter is already in `recv` when exit_group lands.
- **D5 under Hermit:** the number of probes.
- **D6:** the order of the reap and the exit.
- **653:** whether a take or a cancellation commits first depends on host timing, but Linux permits both outcomes.
- **653's end-to-end tests:** which cancellation path runs depends on the spin.
- **Both sides:** cancellation driven by a failure depends on host timing.

## 7. Goalpost-moving

- This analysis does not lower any bar.
- 653 carried three exemptions. The re-land must close them, not port them:
  - the skipped case of a waiter owned by a worker (Codex round 6);
  - test matrices that use only valid arguments;
  - the explicit exemption of argument errors from cancellation (Codex round 7).
- No check on main should be relaxed to accommodate 653's code. `FamilyWaitLedgerMismatch` stays; 653 kept it too, at P executor.rs:3544-3551.

## 8. Limitations and disclosures

**Limitations**

- **Static reading only.**
- **Skill grounding not done by me.** The deterministic-scheduling-review skill asks for background reading: the paper, all of scheduler.rs and the vision documents. I did not do that reading. `.skills/supplemental_docs/hermit-v2-roadmap.md` is missing, as Claude's round-7 review also noted.
- **From memory:** std's mpsc behaviour on EINTR, bash's wait flags, and the kernel's wait semantics.
- **Inferred, not traced:** that detcore serializes D6.
- **3244 evidence gaps:** I read a truncated body of 3244, so its companion commits are unnamed. af5bdb06 cannot be fetched here.
- **The direct runner path was not checked.**
- **653's pull-request body is a snapshot of earlier heads.**

**Noticed in passing, outside 653's scope**

- For `pid < -1`, detcore computes `ProcessGroup(DetPid::from_raw(-pid))` (H threads.rs:1600), where `pid` is a `libc::pid_t` (M reverie-syscalls/src/syscalls.rs:1097).
- For pid == INT_MIN the negation overflows. That panics when overflow checks are on, as they are by default in debug builds, and wraps otherwise.
- Linux returns ESRCH.
- Not run.

**Process disclosures**

- **Scratch files:** I wrote scratch files under /tmp/claude-coord-notes/ and deleted each immediately. All are confirmed absent.
  - Earlier: .cml653-base.txt, .cml653-main.txt and .tmp-cml653-count.diff.
  - In this pass: .tmp-m-exec.rs, a copy of M executor.rs.
- **Failed fetch:** a `git cat-file -t af5bdb06` probe started a lazy promisor fetch in the Hermit repositories. It failed with "not our ref" and wrote no objects.
