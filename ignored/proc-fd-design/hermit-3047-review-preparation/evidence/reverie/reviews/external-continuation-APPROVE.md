I verified every decisive claim against primary source myself rather than accepting either report. Here is the complete final review.

---

# Final follow-up review — KVM proc-fd identity / dynamic fdinfo / mount snapshot

**Bound to** Reverie `696f0476aa46cf29e31b947a89379d80b4542ce3`, tree `b86bfdb0703e96d377007c102eccfa57ba362396`, base `24cd5bb518b027eddb62a226805210d74d31c3d8`, draft https://github.com/rrnewton/reverie/pull/565. Six files: `reverie-kvm/src/{elf.rs,executor.rs,fdinfo.rs,lib.rs,memory.rs,proc_mounts.rs}`.

This follow-up and my earlier complete review form **one** review record. The earlier report's verdict was CHANGES REQUESTED and remains that; it is not retroactively approved. This document supersedes it.

## 1. F1 — withdrawn. My premise was wrong; the gate is correct as written

I read the decisive functions in the retained v6.12 tree directly.

`fs/libfs.c:1633-1659`, `alloc_anon_inode()`:

```c
inode->i_state = I_DIRTY;
inode->i_mode = S_IRUSR | S_IWUSR;      /* line 1653 — no S_IFMT bits */
```

There is no `S_IFREG`. I asserted there was; that was the entire basis of my blocking finding, and it is false.

The rest of the chain confirms nothing re-adds a type:

* `fs/anon_inodes.c:78-125` — `__anon_inode_getfile()` takes either the singleton `anon_inode_inode` (itself built by `alloc_anon_inode` at `:313`) or a fresh `anon_inode_make_secure_inode()` (`:66`, also `alloc_anon_inode`), and passes it to `alloc_file_pseudo`.
* `fs/file_table.c:345-363` — `alloc_file_pseudo()` does `alloc_path_pseudo` + `alloc_file`; it never touches `i_mode`. I grepped the whole file: the only `i_mode` reference is the `S_ISCHR` check in `__fput` at `:432`.
* `fs/eventfd.c:412` — `anon_inode_getfile("[eventfd]", &eventfd_fops, ctx, flags)`, reached from `eventfd2` at `:427`.
* `fs/eventpoll.c:2197` — `anon_inode_getfile("[eventpoll]", &eventpoll_fops, ep, ...)`.
* `fs/pidfs.c:304-313` — `pidfs_getattr()` does `generic_fillattr(...)` then **`stat->mode &= ~S_IFMT;`**, with the comment at `:288` stating this is deliberate ABI: *"User space expects pidfs inodes to have no file type in st_mode."*

So `fstat()` on an epoll / eventfd / timerfd / inotify / signalfd / pidfd descriptor yields `st_mode == 0600`, and `st_mode & S_IFMT == 0`. The candidate's accept set is `{S_IFIFO 010000, S_IFCHR 020000, S_IFDIR 040000, S_IFBLK 060000, S_IFREG 0100000, S_IFSOCK 0140000}` — none is zero, so `ensure_fdinfo_object` (`executor.rs:1382-1395`) returns `ENOSYS`. `fd_mode` (`executor.rs:3542-3550`) is a real `fstat` on the pinned host fd, so this is the actual kernel value.

The comment at `executor.rs:1299-1300` — *"Private anonymous carriers (epoll/inotify/eventfd/timerfd/pidfd, etc.) have no ordinary file type"* — is **literally accurate**, and it names a documented kernel contract (the pidfs comment exists precisely because `lsof` depends on `S_IFMT == 0` for these), not an incidental host observation. My demand for a positive registry or an `anon_inode:` link check is withdrawn: it would add a second mechanism to enforce something the kernel already guarantees.

I also checked the carriers the backend actually creates, to confirm the gate covers them rather than covering them by luck: `epoll_create1` → `libc::epoll_create1` (`:6615`), `eventfd2` → `libc::eventfd` (`:6720`), `timerfd_create` → `libc::timerfd_create` (`:7222`), `pidfd_open` → `SYS_pidfd_open` (`:7306`). All four are genuine host anon-inode/pidfs objects, all four land in `table.files`, and all four are refused by the `S_IFMT` test. Virtual signalfd is refused earlier by `signalfd_fds` (`:1255`).

The one host object the guest can create that *does* have an ordinary type is `memfd_create` (`:5029`, `S_IFREG` on shmem). That one passes the gate — correctly: it is the guest's own object, and its `pos/flags/mnt_id/ino/seals` are its own state. The backend's *supervisor-owned* memfd carrier for synthetic proc files (`:5467`) is separately excluded by `proc_files` at `:1253`. No supervisor-owned host object with an ordinary file type reaches the fdinfo path.

**F1 is resolved with no change required.** The passing test `fdinfo_dispatch_keeps_private_carriers_and_supervisor_procfs_unavailable` corroborates the source; it is not the basis of this finding.

## 2. F5 — the locking nit is withdrawn; the candidate matches Linux exactly

I claimed Linux reads flags, pos, mnt_id and ino atomically under `files->file_lock`, and flagged the candidate's split sampling as a (minor) divergence. `fs/proc/fd.c:23-73` says otherwise:

```c
spin_lock(&files->file_lock);
file = files_lookup_fd_locked(files, fd);
if (file) {
        f_flags = file->f_flags;
        if (close_on_exec(fd, files)) f_flags |= O_CLOEXEC;
        get_file(file);            /* pin */
        ret = 0;
}
spin_unlock(&files->file_lock);    /* line 49 */
task_unlock(task);                 /* line 51 */
...
seq_printf(m, "pos:\t%lli\nflags:\t0%o\nmnt_id:\t%i\nino:\t%lu\n",
           file->f_pos, f_flags,
           real_mount(file->f_path.mnt)->mnt_id,
           file_inode(file)->i_ino);   /* lines 57-60, locks released */
```

Linux samples `f_flags` + `close_on_exec` under the lock, pins with `get_file()`, **releases both locks**, and only then reads `f_pos`/`mnt_id`/`ino`. `observe()` (`executor.rs:1242-1303`) does structurally the same thing: sample `fd_status_flags(host) | guest CLOEXEC` under the one table lock, pin with `F_DUPFD_CLOEXEC`, release table and lifecycle, then read the record from the pinned fd and substitute the sampled flags line. The `F_DUPFD_CLOEXEC` pin is the right analogue of `get_file()` — a dup shares the open file description, so the `pos` read afterwards is the same OFD's, and `mnt_id`/`ino` are per-description/per-inode. The flags substitution is required precisely because the dup carries its own `O_CLOEXEC`.

This is not a divergence to comment on; it is the kernel's own structure. The rest of my F5 — one-directional `sequence → file_table → lifecycle` order, no sequence lock on any mutation path, no lock held across host I/O, in-flight `Arc` clone keeping the description alive, and **no scheduler, turn, virtual-time or RPC interaction anywhere in the diff** — stands unchanged and remains a positive finding.

## 3. F2 — this belongs before the *Hermit* landing, not before the *Reverie* component landing

My prior review made the downstream fixture a precondition of component approval. That demand is unsatisfiable by construction, and I verified the mechanism myself rather than taking the claim.

`scripts/check-reverie-pin.rs:2328-2342` refuses any pin not reachable from `rrnewton/reverie:main`, and `:2385-2403` refuses a pin that does not contain the landing-base pin. I enumerated the production CLI surface (`:187-195`): `--repo`, `--print-pin`, `--base-ref`, `--offline`, `--no-base`, `--staged-pin-advisory`, `--update-to-latest`, `--no-verify-build`, `--help`. There is no candidate-SHA, remote, or ancestry override; `Config.remote` is `#[cfg(test)]` only (`:100-101`). `--no-base` disables monotonicity, not ancestry; `--no-verify-build` skips `cargo check`, not ancestry. `docs/updating-reverie.md:75` states it plainly: *"There is no stale-pin, ancestry, or regression override in testing,"* and `:26`: *"An off-history, backward, or sideways pin cannot produce current validation evidence."* Line 43-44 names the case exactly: ancestry *"rejects abandoned, rewritten, and **unmerged** Reverie commits."*

So requiring the Hermit descriptor-reuse fixture against unlanded `696f0476` demands evidence the maintained policy forbids producing. That is circular, and my earlier review did not account for it.

The second fact that makes the boundary safe rather than convenient: `docs/updating-reverie.md:4` — Hermit depends on Reverie *"pinned to a **specific commit** (`rev = "<hash>"`) rather than a moving `branch = "main"`."* Landing this commit on Reverie main is **inert for every Hermit build** until someone deliberately bumps the pin. Component landing cannot regress Hermit, so nothing is at risk while the downstream work is done in order.

I also confirm the negative: the historical failure is real and stays recorded. KVM refusing fdinfo with `EACCES` was measured on Hermit `7628b6d5` against the old `a2cc` pin. That is a measurement of *the absence of this feature*, on a different pin and a different binary. It is neither a defect in this candidate nor a pass for it, and **no guest, parity, or `cat`-fixture result is inferred anywhere in this review.**

**Verdict on the boundary:** the missing downstream run is *not* a correctness blocker for the Reverie component. I can identify no unresolved mechanism in the six files that only a guest run could settle: every question I raised — anon-carrier exclusion, seq_file semantics, flags formula, lock order, lifecycle binding, copyout counting, mount-capture ordering — is now settled from source, and the component's own behaviour is measured. It is a blocker for https://github.com/rrnewton/hermit/pull/3047.

**Obligations preserved, in order, before that Hermit pull request may land** (stated so they cannot be read as dissolved):

1. Land the reviewed source-preserving rebase onto Reverie main (composition review approves replay of `696f0476` onto `4866241e`, requiring tree `bb2c88a1f0b37e264535693f2b72d77b2502dc5f`; note direct `696f0476` would fail `:2385-2403` because it does not contain base pin `b3049e54`).
2. Bump the Hermit pin with the maintained `--update-to-latest`.
3. Carry the **unchanged** DBT build-budget pin sites. These are a real, documented gap, not boilerplate: `docs/updating-reverie.md:111-119` states `--update-to-latest` does **not** reach them because they are CI shell, not Cargo metadata, and gives the derivation — `git grep -l "$(./ci/run-reverie-pin-check.sh --print-pin)" -- ':!*Cargo.toml' ':!*Cargo.lock'`. The budget must be carried unchanged, not re-baselined.
4. Integrate the separately reviewed Hermit mount-provenance proposal, replacing the current clearing at `hermit-cli/src/lib.rs:1956-1986` — whose own comment (*"The current executor denies `/proc/self/fdinfo/*`"*) is falsified by this change and must not be left stale.
5. Rebuild normally and run the **same bounded descriptor-reuse fixture, unweakened**, plus the followed-stat fixture with its retained prior failure.

If that fixture still fails after step 5, this component review does not excuse it.

## 4. F8 — resolved from source I read myself

My prior review flagged "hermit's namespace setup precedes `load_static_elf`" as an unverified assumption. It is now proved, in the unchanged source rather than an excerpt.

`reverie-process/src/container.rs:855-857`:

```rust
let value = self.setup(&context, &mut []).map(|()| f());
```

`Result::map` does not invoke the closure on `Err`, and `setup` performs mounts (`:704-706`), `chroot` (`:711-713`) and `chdir` (`:716-718`), each with `?`. On the Hermit side, `run.rs:4076-4085` builds the container and then calls `with_container(&mut container, || self.run_in_container(...))`, and `container.rs:1061` routes that through `Container::run`. The callback that reaches `install_static_elf_with_context` therefore cannot run unless the full mount/chroot/chdir sequence succeeded. `ProcMountSnapshot::capture()` at `elf.rs:1033` runs inside that installation path, so it captures the completed namespace. This is ordering proved by control flow, not inferred from command sequence.

The rest of F8 stands: exact bytes, 16 MiB refusal that fails rather than truncates, empty-input rejection, shared `Arc` across fork/exec with guest namespace mutation unsupported, and the mount test replacing a constant that asserted the very behaviour being removed while *adding* `Arc::ptr_eq` lifetime assertions. The `/proc/mounts` limit stays on the record: Detcore normalizes mountinfo via `MountInfoSnapshot` but has no `/proc/mounts` sanitizer, so that file is same-host stable and not cross-machine reproducible — no worse than ptrace, and not fixed here.

## 5. F10 — resolved, with the real limit kept rather than converted into a pass

I could not previously confirm whether `sanitize_fdinfo` handled `lock:` lines. It does — `detcore/src/procfs.rs:2806-2810`:

```rust
} else if body.starts_with("tfd:")
    || body.starts_with("inotify ")
    || body.starts_with("lock:")
{
    return Vec::new();
}
```

So no raw host lock PID / `dev:inode` reaches guest-visible bytes through the normal Detcore path. My leak concern is answered.

**But the honest description of that behaviour is "empty output", not "normalized lock reporting," and it must not be recorded as fidelity.** Concretely: the backend converts guest `F_SETLK` to `F_OFD_SETLK` (`executor.rs:11324-11332`), and OFD locks *do* appear as `lock:` lines in Linux fdinfo. A guest that takes a lock on a file and then reads that file's fdinfo gets **empty content** where Linux returns a record. That is a Detcore-side limit, pre-existing and outside these six files, and passing the line through raw is the correct Reverie/Tool contract — but it is a real gap and stays in the limits list.

Remaining fidelity limits, unchanged and still explicit: `/proc/thread-self/fdinfo/N` records `/proc/<tid>/fdinfo/N` where Linux renders `/proc/<pid>/task/<tid>/...` (the backend has no `task/` surface at all); `/proc/self/fdinfo/N/x` → `ENOENT` where Linux gives `ENOTDIR`; `O_PATH` on fdinfo → `ENOSYS` where Linux succeeds; `ioctl` on a proc file → `ENOSYS` where Linux gives `ENOTTY`; the stale "virtual signalfd" comment at `:8129-8131`; and `observe()` re-implementing `is_open_standard` inline (`:1256-1261`) rather than calling it (`:11193-11198`).

## 6. Findings that were already positive and are unchanged

F3 (raw native object identity is the correct value at the Reverie/Tool boundary, verified against Detcore's actual `determinize_stat` / `namespace.rs` readlink recomputation rather than against backend self-consistency), F4 (seq_file emulation faithful to v6.12 including pread-vs-`f_pos`, retained partial records, `ki_pos == 0` reset, traverse-on-error reset, `copied == 0 → EFAULT`; the `F_SETFL(O_DIRECT)` refusal ordered before flag mutation; the `access_ok`/negative-pread ordering), F6 (fork/thread/exec/exited-leader lifecycle binding matching Linux, including generation-qualified `ENOENT` for a zombie leader), F7 (route-by-route private-carrier isolation, including `vectored_io` returning `ENOSYS` so scalar support does not bypass the vector boundary), and F9 (the `memory.rs` extraction preserving every caller's error value, address, length, and the single atomic `user_access` acquisition) all stand as written.

## 7. Actual execution records

These supersede my earlier "execution pending" statement, which was stale.

| Step | Result | Wall s | CPU s |
|---|---|---:|---:|
| Compile | exit 0, 0 warnings, 0 errors | 5.125 | 7.408 |
| List | exit 0, 427 registered | 0.997 | 0.202 |
| Native selection | exit 0, **37 passed, 0 failed, 0 ignored**, 390 filtered | 1.017 | 0.229 |
| Clippy `-D warnings` | exit 0 | 22.047 | 40.808 |

Clippy payload: `cargo clippy --locked --offline -p reverie-kvm --lib --tests -- -D warnings` on nightly-2026-07-29. Test executable 106,866,600 bytes, `b899a2ee…`, byte-identical across the native and Clippy records. All four services read back inactive/dead, MainPID 0, empty cgroup, complete CPU accounting, no stop or truncation. The 0.03 s in the runner's own line is the internal test phase; the table is the externally accounted envelope.

**Stated limit:** this is 37 of 427 registered tests. `memory.rs::write_user` is on essentially every copyout path and the stat routes lost their synthetic device, so the blast radius exceeds the selection. I checked the most obvious failure mode rather than assuming: the only two surviving test assertions on `SYNTHETIC_GUEST_FD_DEV_MINOR` are at `executor.rs:15503` and `:25004`, inside `captured_output_fstat_is_synthetic_and_stable` and `captured_output_stat_and_link_routes_preserve_the_capture_sink` — **both in the 37, both passing**. No unrun test asserts the removed identity through those constants. Combined with the extraction being behaviour-preserving on the bytes I read, I do not treat the gap as blocking. **Strongly recommended and nearly free:** run the full `-p reverie-kvm --lib` suite — all 427 already compile clean under Clippy, and the 37 cost 0.03 s of runner time, so the full run is cheap and closes the question outright.

## 8. Goalpost-moving assessment

I looked specifically for weakened assertions, widened tolerances, exemptions, skips, relabelling, and deleted checks, and re-read the committed bytes of the one test that changed in order to turn a v2 failure into a v3 pass — the single highest-risk artefact in this change.

`guest_proc_fd_links_and_readlinkat_are_guest_owned`, helper at `executor.rs:23446-23478`: the new oracle calls `libc::fstat` **directly on `state.files[&fd].as_raw_fd()`**, bypassing the backend entirely, and requires *both* the followed `/proc/self/fd/N` stat and the direct guest `SYS_fstat` to equal that native `(st_dev, st_ino, S_IFMT)` triple, plus pins `S_IFMT == S_IFIFO`. The assertions it replaced compared the backend's output to the backend's own synthetic inode — a tautology. **A tautological oracle replaced by an independent one is strengthening.** Every distinctness property survives and several are tightened: alias equality across both pipe ends and the dup (`:23498-23506`, `:23542`), readlink byte-equality *and* length (`:23536-23541`, `:23631-23634`), survivor stability after close with an explicit failure message (`:23569-23572`), and `assert_ne!(new_private_inode, private_inode, "descriptor reuse must allocate a distinct live pipe identity")` (`:23600-23603`), with `assert_pipe_stat_identity` re-applied at every step (`:23532`, `:23568`, `:23623`).

Other checks: `guest_fd_metadata_is_stable_and_isolated_from_supervisor`'s two deleted lines asserted exactly the synthetic device the change deliberately removes, replaced by followed == direct == native. The mount test's constant asserted the behaviour being removed and gained fork/exec lifetime assertions. `sanitize_guest_fd_stat`/`_statx` reducing to timestamp sanitization is justified by Detcore source I verified, not by assertion.

No test was skipped, `#[ignore]`d, renamed, or given a widened comparator. **Nothing was dropped from the selection**: v2's 36 are a subset of v3's 37, and the single v2 failure is present and passing. The v1 `E0061` compile failure, the v2 35/1 result and its executable, the timed-out external sessions, and the old-pin Hermit `EACCES` failure are all retained as failures at their original bindings. The one finding I previously said "would read as goalpost moving if left as-is" — the private-carrier comment labelling rather than checking — is refuted: the comment is accurate and the check is real.

**No goalpost moving found.**

## 9. Verdict

**APPROVE**, bound to `696f0476aa46cf29e31b947a89379d80b4542ce3` / tree `b86bfdb0703e96d377007c102eccfa57ba362396` and the complete six-file source.

Two of my three blocking positions were built on facts I got wrong, and I checked the corrections against primary source rather than accepting the disposition: `alloc_anon_inode` does not set `S_IFREG` (`libfs.c:1653`), so the private-carrier gate works and its comment is true; and Linux's `seq_show` releases `file_lock` before reading `pos`/`mnt_id`/`ino` (`fd.c:49,57-60`), so the candidate's sampling split is the kernel's own. The third, F2, was a real obligation attached to the wrong gate: the maintained pin checker forbids producing that evidence before this commit is on Reverie main, and the `rev = "<hash>"` pin makes landing it inert for Hermit until the pin is deliberately bumped.

This approves the specified change and its bounded component controls. It explicitly does **not** approve, and does not infer: any KVM guest result, any backend parity or determinism claim, the descriptor-reuse or followed-stat Hermit fixtures, the `cat` diagnostics, Hermit's reader-DetFd fdinfo normalization, Detcore's proc-snapshot rewind limitation, or the Hermit mount-provenance proposal. The five downstream obligations in §3 remain in force before https://github.com/rrnewton/hermit/pull/3047 may land, and the old-pin `EACCES` failure stays on the record as a failure until that fixture is re-run unweakened and passes.

Non-blocking, recommended in a follow-up rather than as a condition: run all 427 library tests (§7); correct the stale `translate_outgoing_control` comment; call `is_open_standard` in `observe()` instead of duplicating it; add a non-empty `requested_end` overflow case to the copyout test; and record at `elf.rs:1033` that the mount snapshot is never refreshed. The `hermit-cli/src/lib.rs:1956-1986` comment asserting that the executor denies `/proc/self/fdinfo/*` becomes false the moment the pin moves and must be fixed as part of step 4, not later.