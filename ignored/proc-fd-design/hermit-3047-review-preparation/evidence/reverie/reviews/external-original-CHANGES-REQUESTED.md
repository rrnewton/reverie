# Independent Claude review — KVM proc-fd identity / dynamic fdinfo / mount snapshot (source-v3)

Base `24cd5bb518b027eddb62a226805210d74d31c3d8`, branch `codex/kvm-proc-fd-identity-20260917`.
Review root: `.../slots/kvm-proc-fd-identity-20260917/ignored/proc-fd-design/source-v3/files/`.

## 1. Exact source reviewed

Read in full: `reverie-kvm/src/fdinfo.rs` (122 L), `reverie-kvm/src/proc_mounts.rs` (85 L), `reverie-kvm/src/lib.rs` hunk.
Read in the candidate `executor.rs` (targeted, by symbol): 1180–1600, 1602–1730, 2100–2280, 2700–2740, 3340–3420, 3400–3530, 3526–3560, 3970–4090, 4340–4380, 4561–4625, 5030–5150, 5520–5700, 5840–5900, 8106–8145, 8955–8990, 9240–9330, 9311–9500, 9491–9660, 10635–10670, 10860–10940, 11080–11215, 11208–11400; tests 23380–23680, 25100–25270.
Read in candidate `elf.rs`: 560–742, 960–1042, 200–260, 322–360.
Read in candidate `memory.rs`: 300–600 (all of `write_user`/`write_user_prefix`/`write_raw`/`copy_to_user*`/`put_user_i32`), plus the new test.
Primary Linux: retained `linux-v6.12/seq_file.c` (`traverse`, `seq_read_iter`, `seq_lseek`), `linux-v6.12/fd.c` (`seq_show`, `proc_fdinfo_file_operations`, `tid_fd_*`), and `.../source-v1-review/linux-v6.12/fcntl.c` (`setfl`, `SETFL_MASK`, `F_GETFL`).
Hermit side (read-only context): `detcore/src/syscalls/namespace.rs` (anonymous proc-fd readlink canonicalization), `detcore/src/syscalls/files.rs` (`determinize_stat`, `handle_stat_family`, `handle_statx`, `handle_fcntl`), `detcore/src/procfs.rs` (`ProcfsKind::Fdinfo`/`sanitize_fdinfo`, `MountInfoSnapshot`).

I re-read every region I cite after first pass; I observed no intervening change in the bound files during the review. I could not re-authenticate the six SHA-256 hashes myself (no Bash); the launcher owns that.

---

## 2. Findings, most important first

### F1 — BLOCKING. `ensure_fdinfo_object` cannot exclude anon-inode private carriers; its comment states a false Linux fact
`executor.rs:1382-1395`, comment at `executor.rs:1298-1300`.

```rust
fn ensure_fdinfo_object(host_fd: RawFd) -> Result<(), i64> {
    if !matches!(fd_mode(host_fd)? & libc::S_IFMT,
        S_IFREG | S_IFDIR | S_IFCHR | S_IFBLK | S_IFIFO | S_IFSOCK) { return Err(ENOSYS) }
```
with `// Private anonymous carriers (epoll/inotify/eventfd/timerfd/pidfd, etc.) have no ordinary file type`.

On Linux, `fs/libfs.c:alloc_anon_inode()` sets `inode->i_mode = S_IFREG | S_IRUSR | S_IWUSR`, and every `anon_inode_getfile()` file shares that inode. So `fstat()` on an epoll / eventfd / timerfd / inotify / signalfd descriptor reports **`S_IFREG`**, which is in the accept set. The gate is therefore not the mechanism the comment claims. (The retained Linux tree here contains only `fd.c`, `seq_file.c`, `fcntl.c`, `read_write.c`, `shmem.c` — `libfs.c` is not retained, so I could not confirm from the bound primary source, and I have no probe.)

This matters because the backend creates *real* host carriers: `epoll_create1` at `executor.rs:6608` and `eventfd2` at `executor.rs:6714` (`libc::eventfd(...)`). Neither is in `proc_files`, `random_device_fds`, or `signalfd_fds` (the latter is populated only from `process_signals.signalfd_masks`, `executor.rs:1590-1597`). So for a guest epoll fd the only refusal is this S_IFMT test, both at open (`fdinfo_target_generation`, `executor.rs:1496`) and per observation (`observe`, `executor.rs:1301`).

Exactly two outcomes, and I cannot distinguish them without execution:

* **If anon inodes are `S_IFREG` (my reading of Linux):** the new test `fdinfo_dispatch_keeps_private_carriers_and_supervisor_procfs_unavailable` (`executor.rs`, the `[(SYS_eventfd2, …), (SYS_epoll_create1, …)]` loop asserting `ENOSYS`) **fails**, and the underlying isolation defect is real: `read("/proc/self/fdinfo/<epollfd>")` would return the supervisor's `tfd: <raw supervisor fd> events: … data: … pos: … ino: …` lines — precisely the "supervisor epoll target numbers" the acceptance criteria forbid — plus `eventfd-count:`/`eventfd-id:` for eventfd.
* **If the target kernel reports some other `S_IFMT`:** the test passes, but the isolation is *incidental*, rests on an unstated kernel detail, and the in-code justification is wrong.

Counter-evidence I weighed: this test is byte-identical between v2 and v3 (only `guest_proc_fd_links_and_readlinkat_are_guest_owned` changed), it is one of the 17 new tests, and v2 reportedly ran 36 methods with only the pipe test failing. That is a real signal against my reading — but it is a second-hand report, v3's own run is pending, and the code comment is still wrong either way.

**Required change:** do not classify private carriers by `S_IFMT`. Use a positive registry (the backend already knows which guest fds it created as epoll/eventfd/timerfd/pidfd/inotify — track them the way `signalfd_fds` is tracked), or reject any target whose `/proc/self/fd/<raw>` link begins with `anon_inode:`. Whatever is chosen, replace the comment with the actual observed mode and add an assertion pinning it.

### F2 — Approval cannot clear the known-failing downstream fixture
The descriptor-reuse Hermit integration fixture currently fails at **KVM fdinfo EACCES**, and the broader followed-stat fixture has a retained prior failure. This change is plausibly the fix for the EACCES (the backend now implements `/proc/PID/fdinfo/N` open instead of refusing it), but that is *inference*, not a result: v3's Rust selection has not been executed, the pinned-image loader case is unverified, and the host `cat` diagnostic is a host-helper result, not a guest/parity result. Per the review rule, no approval may treat these as cleared.

### F3 — Correct, and the central question answered: replacing synthetic pipe/socket readlink IDs with native object IDs is right at this boundary
`executor.rs:5605-5620` (readlink), `9252-9272` (`guest_object_stat`), `11090-11104` (`sanitize_guest_fd_stat`/`_statx` reduced to timestamp sanitization only).

Traced to actual Hermit normalization:

* `detcore/src/syscalls/namespace.rs:302-319` — for a **own-process** `/proc/self/fd/N` readlink, Detcore does **not** parse the backend's text. It injects `fstat(fd)`, takes `stat.st_ino`, and rewrites the buffer as `format!("{kind}:[{inode}]")` using `deterministic_stdio_inode_for_*` or `determinize_inode(guest, stat.st_ino)`. So the backend's readlink bytes are overwritten regardless.
* `detcore/src/syscalls/files.rs:2606-2686, 2689-2708` — `handle_stat_family`/`handle_statx` under `virtualize_metadata` inject the real call and then `determinize_stat`, which maps **both** `stat.inode` (`determinize_inode`) and `stat.dev` (`determinize_device`).

Consequence: under the *base*, direct `fstat` already returned the native inode (only timestamps sanitized) while the followed `/proc/self/fd/N` path returned a synthetic one. Detcore determinizes each raw value independently, so the two produced **different virtual inodes for the same object** — the identity bug. The candidate makes every route deliver the same raw kernel identity, which Detcore then maps to one virtual inode, and `determinize_device` already exists so the removed synthetic `st_dev` is not a new leak. This is the ptrace-reference contract (raw from the kernel, virtualized by the Tool), so it is a parity improvement, not a bypass. The test-only reviewer's concern is answered concretely, not by native self-consistency.

Residual, pre-existing and *not* caused by this change: `inode_override` (`files.rs:2668-2677`) is applied only for `StatFamily::Fstat`, never for `newfstatat`/`statx`. So for a **captured-output** fd, `fstat` may take the stdio override while the followed path takes `determinize_inode(synthetic)`. The candidate preserves the captured sink's existing synthetic identity on all routes (`9257`, `9404-9413`, `9573-9586`, `5579-5585`), which is what was asked; the asymmetry is Detcore's.

### F4 — seq_file emulation is faithful to Linux v6.12
`fdinfo.rs` against retained `seq_file.c`:

| Linux | Candidate | Verdict |
|---|---|---|
| `if (!iov_iter_count(iter)) return 0;` (`seq_read_iter:179`) | `if count == 0 { return 0 }` (`fdinfo.rs:53`) | match |
| `ki_pos == 0 → index=0, count=0` (`:188`) | `offset == 0 → reset()` (`:56`) | match (`end=false` ≙ `index=0`) |
| `ki_pos != read_pos → traverse` w/ error reset `read_pos=0,index=0,count=0` (`:194-205`) | `:59-66` | match |
| `traverse`: `offset==0` returns early; else show once, `from = offset-pos`, `count -= from`; `offset>=len → count=0, index=1` (`:90-130`) | `:28-40` (`from = min(offset,len)`, `end=true`) | match for a single-record file |
| leftover `m->count` copied before fetching a new record (`:215-222`) | `if from == len && !end { observe }` then copy remainder | match |
| `copied==0 && m->count → -EFAULT`, else `err` (`:286-287`) | `Ok(0) → EFAULT`; `available==0 → 0` | match |
| `ki_pos += copied; m->read_pos += copied` only on success; pread's `ki_pos` is a local | `from/read_pos += copied`; `file_pos` only when `positioned_offset.is_none()` (`:88-92`) | match |
| `seq_lseek`: SEEK_END → `-EINVAL`; negative → `-EINVAL`; `offset != read_pos → traverse`; error → `f_pos=0,read_pos=0,…` (`:308-341`) | `:96-121` | match (plus `checked_add` hardening on SEEK_CUR, stricter than Linux's wrap) |

`seq_show` (`fd.c:23-73`) confirms the record is `pos/flags/mnt_id/ino` + `show_fd_locks` + `f_op->show_fdinfo`, that a missing task **or** missing fd yields `-ENOENT`, and that `f_flags = file->f_flags | (close_on_exec(fd) ? O_CLOEXEC : 0)`. `fcntl.c:469-471` confirms `F_GETFL` returns exactly `filp->f_flags`, so `observe()`'s `fd_status_flags(host) | guest CLOEXEC` (`executor.rs:1283-1288`) is the kernel's own formula, and `replace_fdinfo_flags` correctly substitutes only that line so the supervisor's internal `O_CLOEXEC` dup flag never leaks.

The `F_SETFL(O_DIRECT)` correction (`executor.rs:11300-11305`) matches `fcntl.c:62-66,83-86`: `setfl` rejects with `EINVAL` **before** `filp->f_flags = …`, so the refusal must be atomic and must not clear `O_NONBLOCK`. The candidate returns before touching the carrier. Correct.

The scalar-read ordering correction (`executor.rs:1306-1318`) matches `ksys_pread64` (`pos < 0 → EINVAL`) preceding `vfs_read`'s `access_ok(buf, count)`, which precedes `rw_verify_area`'s count clamp. Negative-pread ordering is preserved; `validate_guest_iovec_address` (`4353-4365`) is the x86-64 `access_ok` rule and runs before `count.min(MAX_HOST_IO)` and before any sequence effect.

### F5 — Lock order and progress are sound; no new scheduling artefact
`observe()` (`1237-1304`) takes **one** table lock, then lifecycle, samples flags, pins the target with `F_DUPFD_CLOEXEC`, and **releases both before** `ensure_fdinfo_object` + `read_owned_fdinfo`. The pin is load-bearing: it stops the supervisor reusing the raw fd number between sampling and `/proc/self/fdinfo/<raw>`.

Order is exactly one direction everywhere: `sequence → file_table → lifecycle`. The sequence lock is reachable only from `read`/`pread64`/`lseek` dispatch, and none of those is in `mutates_file_table` (`1642-1694`), so `execute()` has already done `shared_files.take()` (`3452-3454`) before dispatch. Mutation paths hold the table but never take the sequence lock: `dup`/`dup2`/`dup3`/`F_DUPFD*` only clone the `Arc` (`6105-6107`, `6184-6211`, `11237/11250`), `close` only removes it (`11403`), `open_fdinfo` locks lifecycle *under* the table (same order, `1481-1487`). `replace_after_exec` (`2709-2736`) drops the lifecycle temporary before locking the table. No deadlock, no second table lock, no sequence lock on a mutation path.

The in-flight `Arc` clone in `read`/`pread64` (`3980`, `4028`) keeps the description alive even if a sibling closes and reuses the guest fd mid-syscall. `lseek` uses a borrow of the owning `&LoadedStaticElf`, which is equally safe.

No scheduler event, turn, virtual-time shortcut, RPC cancellation, or host-timed scheduling decision is introduced anywhere in the diff. Confirmed by inspection of the whole diff.

Minor non-atomicity worth one line of comment: `flags` is sampled under the table, while `pos`/`ino`/`mnt_id` are read from procfs after release. Linux's `seq_show` reads all four under `files->file_lock`. Under Detcore's sequentialized scheduling a single turn owns the syscall, so this is not currently observable, but it is a divergence from the kernel's atomicity.

### F6 — Lifecycle: original-target binding across fork/exec/exit is correct
* Fork (`elf.rs:594-606`): the child clones the `Arc<FdinfoDescription>`, which keeps the **parent's** `Weak<FileTableState>` and `target_tid`; the child's own `fdinfo_table` weak is repointed at its fresh table (`executor.rs:2141-2142`). Correct — the open description still names the parent's fd.
* Thread (`executor.rs:2213-2249`): shares the same `file_table` `Arc`, and `try_clone_for_fork` copies the weak that already points at it. Correct.
* Exec (`elf.rs:641-645, 736-738`; `executor.rs:2709-2736`): `fdinfo_files` is filtered by the surviving (non-CLOEXEC) `files`, the table `Arc` is retained and repopulated, and `reset_after_exec` (`elf.rs:322-334`) **preserves the task generation**, so a surviving description stays valid — which matches Linux, where exec does not invalidate `/proc/PID/fdinfo/N`.
* Exited leader (`observe`, `1247-1252`): generation-qualified lookup returns `ENOENT`. This matches Linux exactly: a zombie leader has `task->files == NULL`, so `seq_show` (`fd.c:36,54-55`) returns `-ENOENT`. A live worker's table is *not* borrowed. Numeric TID reuse with a new generation does not resurrect the description.

### F7 — Private-carrier isolation, route by route (other than F1)
Checked every reachable route:
* fdinfo readlink → recorded `/proc/TID/fdinfo/N` (`5589-5591`), placed **before** the `proc_files` branch — load-bearing, because `synthetic_proc_path_for_inode` would return `None` for this inode and fall through to `canonical_fd_path`, which would print `/memfd:reverie-kvm-proc (deleted)`.
* fstat/newfstatat/statx on the fdinfo fd → `proc_files` synthetic path stat/statx (`11091`, `9465-9473`, `9540-9556`). No memfd inode, size 0, `S_IFREG`.
* write/pwrite → carrier is re-opened `O_RDONLY`, so `ensure_writable` → `EBADF`.
* readv/writev → `vectored_io:4595` returns `ENOSYS` for any `proc_files` fd. **The scalar support does not bypass the vector boundary.**
* mmap / sendfile / ioctl / fstatfs / fsync / fdatasync / readahead / sync_file_range / fchmod / fchown → `fdinfo_private_carrier_error:1343-1380` `ENOSYS`.
* fcntl other than DUPFD/DUPFD_CLOEXEC/GETFD/SETFD/GETFL/SETFL → `ENOSYS` (`1365-1378`).
* `open("/proc/self/fd/<fdinfo fd>")` → `open_guest_fd_path:5633-5635` `ENOSYS`, before host-fd resolution, so O_PATH cannot smuggle the carrier out.
* SCM_RIGHTS out → `translate_outgoing_control:8126-8132` `ENOSYS`.
* `truncate("/proc/self/fd/<fdinfo fd>")` → `EACCES` (`5102-5104`); `ftruncate` → read-only carrier error.
* splice/tee/copy_file_range are not implemented at all, so no route.
* Supervisor fds are unreachable by number: `fdinfo_path_target` only accepts the guest's own pid/tid spellings and the fd is resolved through the guest table (test dups a supervisor file to ≥1000 and asserts `ENOENT`).

Two nits: the comment at `8129-8131` still says "virtual signalfd" though it now also covers fdinfo; and `ioctl` on a proc file returns `ENOSYS` where Linux returns `ENOTTY`. Explicit-unsupported, acceptable, but worth naming.

### F8 — Mount snapshot: correct in shape, with one coupling risk and one staleness note
`proc_mounts.rs` captures exact bytes with a 16 MiB capacity refusal that fails rather than truncating, rejects empty input, and propagates read errors — verified by its two tests. Capture happens once at `load_static_elf` (`elf.rs:1033`), in the supervisor that performs the guest's host-path syscalls, so it is the right namespace **provided** hermit's container/namespace setup precedes ELF load; I could not verify that ordering from the bound files and flag it as an assumption the integration fixture must confirm. It is never refreshed: `try_clone_for_fork` and `inherit_process_state` share the same `Arc` (`elf.rs:602`, `736`), which is right for guest-initiated changes (unsupported) but would go stale if the *supervisor* mounted anything after load.

Coupling risk to state plainly: this replaces a fixed deterministic single row with real host bytes for **both** `/proc/self/mountinfo` and `/proc/mounts`/`/proc/self/mounts` (`10652-10656`). Detcore has a full `MountInfoSnapshot` normalizer (`detcore/src/procfs.rs:398-600`, raw→virtual mount IDs, device pool, peer groups, root rewrites, `mount_ids_are_ordered_subset`) — so mountinfo is the intended input to an existing consumer. I found **no** Detcore sanitizer for `/proc/mounts`; under ptrace that file is already host-real, so same-host `--verify` determinism holds and parity improves, but cross-machine reproducibility does not. The Hermit proposal is correctly treated as separate context here.

The mount test change (`executor.rs:15217-15258`) replaces `EXPECTED = "1 0 0:1 / / rw - rootfs rootfs rw\n"` with a comparison against the real `/proc/self/mountinfo`, and **adds** fork/exec `Arc::ptr_eq` lifetime assertions while keeping the synthetic inode / `synthetic_proc_path_for_inode` / `st_size` identity checks. Not a weakening — the old constant asserted the very behaviour being removed.

The pipefs assertion in `fdinfo_dispatch_tracks_ordinary_replacements_…` (`assert_ne!(field(bytes,"mnt_id:",10), 0)`) correctly preserves a genuine unlisted ID rather than whitelisting or zeroing it.

### F9 — `memory.rs` extraction is behaviour-preserving for existing callers
`memory.rs:427-471`. `write_user` now delegates to `write_user_prefix` and re-raises `GuestMemoryAccessDenied { address: guest_address, length: source.len() }` — the **original address and complete requested length**, unchanged. Empty source: base returned `Ok(())`; new returns `Ok(0)` and `0 == source.len()` → `Ok(())`. Identical.

The permission lock (`user_access`) is acquired once and held across the writable-prefix walk **and** the `write_raw` call, which takes `host_access` inside — same single acquisition, same order, so the permission observation stays atomic. `partial` still governs the "write the prefix anyway" side effect, so `copy_to_user`'s legacy partial write is intact and `put_user_i32` remains all-or-nothing. `write_raw`, `zero_raw`, and public `write`/`read`/`zero` are untouched; the adjacent `prctl_copyout_keeps_privileged_write_and_scalar_contracts` test is retained, so the privileged/internal paths gain nothing.

The new counted API is a genuine counted copy, not "probe then write": the count returned is the same `length` that governed the single `write_raw`. The read cursor advances only by `copied` (`fdinfo.rs:88-89`), and the end-to-end partial-fault case is exercised by `fdinfo_dispatch_pread_and_guest_faults_…` (7 bytes copied into the last 7 bytes of a writable page, cursor 7, next read returns `original[7..]`), which is exactly Linux's retained-partial-record behaviour.

Cases covered by the new test: empty input (`u64::MAX`, `Ok(0)`), initial denial (`Ok(0)`, no write), mixed writable/read-only (`Ok(7)`, exact bytes), scalar all-or-nothing preserved, legacy `copy_to_user` partial side effect preserved, full 14 after remapping, physical end (`2*PAGE` → `Err`). **Gap:** the `requested_end.checked_add` overflow branch is only reached via the empty short-circuit path, never with a non-empty buffer; `checked_offset(addr,1)` makes it nearly unreachable anyway, but the prompt asked for it and it is untested. Non-blocking.

### F10 — Smaller fidelity divergences (none blocking)
* `/proc/thread-self/fdinfo/N` records its readlink target as `/proc/<tid>/fdinfo/N`; Linux would render `/proc/<pid>/task/<tid>/fdinfo/N`. The backend has no `/proc/PID/task/...` surface at all, so this is consistent with existing limits.
* `/proc/self/fdinfo/N/x` and `/proc/self/fdinfo/N/` → `ENOENT` where Linux gives `ENOTDIR`.
* `open(..., O_PATH)` on fdinfo → `ENOSYS` where Linux succeeds; explicit-unsupported, mirroring the existing `open_guest_fd_path` O_PATH refusal.
* `observe()` re-implements `is_open_standard` inline (`1256-1261`) rather than calling it (`11193-11198`). Logic matches today; it is a duplication that can drift.
* `show_fd_locks` output is passed through verbatim, including `dev:inode` and pid columns. The backend converts guest `F_SETLK` to `F_OFD_SETLK` with `l_pid = 0` (`11324-11332`), so this is narrow, but it is raw host state reaching the Tool and I did not confirm `sanitize_fdinfo` handles `lock:` lines.

---

## 3. Determinism / progress conclusions

**Determinism.** No guest-visible value is newly exposed that the Tool does not already own a normalizer for, with one exception to watch. Inodes/devices from every stat route: `determinize_stat` (`files.rs:2606-2653`). Pipe/socket readlink: recomputed by Detcore from an injected `fstat` (`namespace.rs:302-319`), so the backend's bytes are overwritten. fdinfo `mnt_id:`/`ino:`: `ProcfsKind::Fdinfo` → `sanitize_fdinfo` (`procfs.rs:646,714,943`). mountinfo: `MountInfoSnapshot`. The exception is `/proc/mounts`, for which I found no Detcore sanitizer — same-host stable, not cross-machine reproducible, and no worse than ptrace.

**Progress.** No lock is held across host I/O; the sequence lock never nests under the file table; every mutation path is sequence-free. The only new blocking primitives are non-blocking `fcntl`/`fstat`/`open`/`read` on procfs. No scheduler interaction of any kind is introduced.

## 4. Goalpost-moving assessment

I looked specifically for weakened assertions, widened tolerances, exemptions, skips, relabelling, and deleted checks.

* **`guest_proc_fd_links_and_readlinkat_are_guest_owned`** (the only v2→v3 change): the removed assertions compared the backend's output to *its own* synthetic inode — a tautology. They are replaced by `assert_pipe_stat_identity`, which compares both the followed stat and the direct fstat against an **independent native `libc::fstat` on the owned host descriptor**, including `st_dev` and `S_IFMT`. Every distinctness property is kept and some are strengthened: private-alias equality across read/write/dup ends, `new_private_inode != private_inode` on reuse, survivors retaining `stable_inode` after close, alias equality of the new pipe's two ends, readlink byte-equality *and* length. Isolation (`/proc/self/fd/99` → `ENOENT`), the memfd non-leak (`/proc/1/status`), nofollow and captured-output controls are all still present (the last two in their own tests). **Not goalpost moving** — this is a tautological oracle replaced by an independent one.
* **`guest_fd_metadata_is_stable_and_isolated_from_supervisor`** (`25168-25190`): the two deleted lines asserted `stx_dev_major == SYNTHETIC_DEV_MAJOR` / `stx_dev_minor == SYNTHETIC_GUEST_FD_DEV_MINOR`, i.e. exactly the behaviour the change deliberately removes. They are replaced by followed == direct == native. nofollow `S_IFLNK`, missing-fd `ENOENT`, and stat/statx cross-consistency are untouched. **Not goalpost moving.**
* **Mount test**: constant replaced by real bytes, with added fork/exec lifetime assertions; synthetic identity checks retained. **Not goalpost moving.**
* **`sanitize_guest_fd_stat`/`_statx`** now only sanitize timestamps. This is a production simplification whose justification (Detcore owns inode/device virtualization) I verified in Detcore source rather than taking on assertion. **Not a silent relaxation.**
* No test was skipped, `#[ignore]`d, renamed to hide a failure, or given a widened comparator anywhere in the diff.

One thing that *would* read as goalpost moving if left as-is: F1's comment asserts a Linux property that justifies a weak type-based gate. If the gate is in fact inert for anon inodes, then the "private carriers are excluded" claim is a label rather than a check.

## 5. Evidence inspected and its limits

Static source reading only — no build, no test run, no guest, no probe. Specifically **not** established by me: (a) that v3 compiles; (b) that the 37-method selection passes (its execution is pending, and v2's 35/36 is a second-hand report for a different tree hash); (c) `fstat().st_mode` of a host epoll/eventfd/timerfd/pidfd on the target kernel — the decisive fact for F1, with `libfs.c` absent from the retained Linux tree; (d) that hermit's namespace setup precedes `load_static_elf`; (e) any Hermit integration result. The host `cat` diagnostic and the unverified pinned-image loader case contribute nothing to a pass.

## 6. Verdict

**Changes requested.**

The repair is, in substance, well built. The seq_file emulation is a faithful port of v6.12 semantics including the awkward parts (pread vs `f_pos`, retained partial records, `ki_pos == 0` reset, traverse-on-error reset, `copied == 0 → EFAULT`); the flags formula is the kernel's own; the O_DIRECT and access_ok corrections are right and correctly ordered; lifecycle binding across fork/exec/leader-exit matches Linux; lock order and progress are clean; the `memory.rs` extraction preserves every existing caller, error value and lock property; and the pipe/socket identity change is the correct answer at the Reverie/Tool boundary, verified against Detcore's actual normalization rather than against native self-consistency. I found no goalpost moving.

Blocking before approval:

1. **F1** — replace the `S_IFMT` private-carrier gate with a mechanism that actually excludes anon-inode descriptors (positive registry, or `anon_inode:` link check), and correct the false comment. Resolve which of the two F1 outcomes holds by running `fdinfo_dispatch_keeps_private_carriers_and_supervisor_procfs_unavailable`; if it currently passes, the mechanism is still wrong-by-accident and should be made explicit.
2. **F2** — the v3 Rust selection (all 37) must be executed green, and the descriptor-reuse Hermit fixture that fails at KVM fdinfo EACCES must be re-run. No approval can treat either as cleared by inference.

I am not demanding an exact-head whole-DAG receipt; the two named checks above plus a compile are sufficient, consistent with the soft-green rebase landing directive.

Non-blocking, recommended in the same change: correct the stale `translate_outgoing_control` comment (F7); call `is_open_standard` instead of re-implementing it in `observe()` (F10); note the flags-vs-record sampling non-atomicity (F5); add a non-empty overflow case to the `copy_to_user_prefix` test (F9); and record, where the mount snapshot is captured, that it is never refreshed and assumes capture-after-namespace-setup (F8).

Explicitly **not** approved or assessed as complete: dynamic fdinfo parity in general, Hermit's reader-DetFd fdinfo normalization, Detcore's proc-snapshot rewind limitation, and the Hermit mount-provenance proposal — all outside this change.
