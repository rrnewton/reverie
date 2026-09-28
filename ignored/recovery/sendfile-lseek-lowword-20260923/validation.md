# sendfile/lseek low-word candidate validation

Pre-rebase base: `dc7dac97995fee2393d2bcef147116134314781a`

Pre-rebase commit: `e2461c2fa23dfada20057196eb67560a71e8f24e`

Two-file diff SHA-256: `511772f7e6dfa30a8a3337b7ce1088356a0f5c1f625915d507ca06e8af17ee8a`

## Scope

- Decode `sendfile` output fd from its low 32-bit signed word.
- Decode `sendfile` input fd from its low 32-bit signed word.
- Decode `lseek` fd from its low 32-bit signed word.
- Preserve all downstream routing and validation order.

Excluded pre-existing behavior:

- `sendfile` reads a non-null offset pointer after fd resolution, unlike Linux.
- regular-file `lseek` rejects high bits in `whence`, unlike Linux's low-word decoding.
- captured-output `lseek` returns `ESPIPE` before invalid-whence validation, unlike a native pipe.

## Green cells

- `cargo fmt --all -- --check`
- `git diff --check`
- focused unit: 1/1, 815 filtered out
- required real KVM: 1/1, 340 filtered out; native once, direct KVM twice, Tool KVM twice; exact stdout/stderr/exit
- full library serial: 816/816 in 31.38 s
- full library parallel rerun: 816/816 in 11.41 s
- all-target clippy with `-D warnings`: green in 18.89 s

One earlier full-parallel run remains recorded red: 815/816, with `descriptor_retirement_accept_cleanup_releases_both_guards` failing on host `EAGAIN` (`Resource temporarily unavailable`). It was followed by the clean serial and parallel runs above; it was not relabelled or deleted.

## Mutation evidence

Each old decoder was restored alone, and then the candidate was restored byte-for-byte:

- old sendfile output decoder: unit failed `-9` vs `2`; KVM guest exited 8
- old sendfile input decoder: unit failed `-9` vs `2`; KVM guest exited 9
- old lseek decoder: unit failed `-9` vs `6`; KVM guest exited 6

Final restored focused unit and required KVM tests passed. Final candidate diff hash returned to `511772f7e6dfa30a8a3337b7ce1088356a0f5c1f625915d507ca06e8af17ee8a`.

## Preliminary adversarial review

Two independent Codex reviews approved the exact pre-rebase diff hash with no blocking finding and no goalpost moving. Any rebase or source change invalidates those exact-artifact approvals and requires revalidation/review.

## Post-rebase exact evidence

Base: `7bc49f4c4d63018adba61d49246e847569518e33`

Head: `730123acb922cfa30fa1bb9281391776088fb35d`

Frozen artifact: `sendfile-lseek-frozen-post-rebase.diff`

Frozen SHA-256: `460ccbcaa5d5262c977179e2a8391ac59d05db76042168fcc6af05a9875caa8a`

- `cargo fmt --all -- --check`: green
- `cargo clippy -p reverie-kvm --all-targets -- -D warnings`: green
- focused unit: 1/1 green, 816 filtered out
- required real KVM: 1/1 green, 341 filtered out; native once, direct KVM twice, Tool KVM twice; exact stdout/stderr/exit
- full library serial: 817/817 green in 33.54 s
- full library parallel: third root-owned run 817/817 green in 8.27 s

Two earlier post-rebase parallel runs remain recorded red at 816/817: one unrelated `positioned_vectored_io_handles_pipes_partial_writes_and_sigpipe` mismatch and one unrelated `descriptor_retirement_accept_cleanup_releases_both_guards` host `EAGAIN`. The clean exact-head run satisfies the stated alternative release gate without erasing or causally disproving those flakes.

Two independent final Codex reviews approved this exact head and artifact with no blocking defect and no goalpost moving. The stable patch ID is `0a0aa800b777e5801a9d802403f97ab703db304e` across the rebase.

## External-review correction

The external review of head `730123acb922cfa30fa1bb9281391776088fb35d`
requested changes: after low-word decoding, a negative `sendfile` output fd was
not rejected until after input routing. Live pipe, socket, directory,
standard-stream, and ordinary procfs inputs could therefore return the KVM
fallback sentinel `ENOSYS` instead of `EBADF`.

Correction commit and new exact head:
`5d65b9f50f124f615c5f0c862af7b94919037c3f`.

- Full base-to-head diff artifact: `sendfile-lseek-frozen-review-fix.diff`
- Full diff SHA-256: `2b8dbdfb356d055e3573aecc23cd0dbb02f0a4c0bb0e74b2f75b71febdd6f88a`
- Incremental correction artifact: `negative-outfd-review-fix.diff`
- Incremental correction SHA-256: `c1847753c83977877f2d7ff5186b9f4a2c5f2625f3e73bab4bfd8b5749380dfc`
- Remote branch readback matched the local head and both changed files byte-for-byte.

The correction rejects a negative low-word `out_fd` immediately inside
`sendfile`, before ordinary input lookup or classification. Unit coverage uses
both `0x5a5a5a5a80000001` and sign-extended `-1` against pipe, socket,
directory, stdout, and real procfs inputs. The real-KVM fixture crosses both
encodings with pipe and AF_UNIX socket inputs, with `offset=NULL` and count 1.

Exact-head gates:

- `cargo fmt --all -- --check`: green
- `cargo clippy -p reverie-kvm --all-targets -- -D warnings`: green
- new focused unit regression: 1/1 green
- original focused low-word unit: 1/1 green
- required real-KVM fixture: 1/1 green; native once, direct KVM twice, Tool KVM twice; exact stdout/stderr/exit
- full library parallel: 818/818 green in 8.84 s
- full library serial: 818/818 green in 26.63 s

Removing only the new production guard killed both required regressions: the
unit observed `ENOSYS` (`-38`) instead of `EBADF` (`-9`), and the KVM guest
exited 22 on its first pipe case. Restoring the guard returned both to green.

Two independent Codex reviews approved full diff SHA-256 `2b8dbdfb...` with no
goalpost moving. A fresh external exact-head review remains mandatory before
merge.

Pre-existing residual: `fdinfo_private_carrier_error` runs before `sendfile`,
so a private synthetic `/proc/*/fdinfo` input can still return `ENOSYS` before
the in-function negative-output check. This was already true on base
`7bc49f4c`; the correction's procfs regression case uses a real host procfs
descriptor. No broader private-carrier ordering claim is made here.

## Positive output-alias ordering correction

The external review of head `5d65b9f50f124f615c5f0c862af7b94919037c3f`
found that positive decoded aliases of unusable outputs still reached input
classification first. Closed, read-only regular, and pipe-read-end outputs
could return `ENOSYS` for otherwise-live unsupported inputs instead of the
native/base `EBADF`.

Correction commit and new exact head:
`79f139bd7e1b8b789e1b770597c03b9f0bbd6050`.

- Full base-to-head artifact: `sendfile-lseek-frozen-positive-output-fix.diff`
- Full diff SHA-256: `af4e2e7f20c4262b3e0cc4eca4ada9382c55a2660da555bcca985564ca538d9c`
- Incremental artifact: `positive-output-ordering-frozen.diff`
- Incremental SHA-256: `30777bf3ee93ac725b48e10f55e68f0110713e5ace560feb6efa45beca5a4b41`
- Author and committer: `rrnewton-bots <330061689+rrnewton-bots@users.noreply.github.com>`

After low-word decoding, `sendfile` now requires the output to be present in
the modeled file table and writable, or to satisfy the existing open-standard
descriptor contract, before any input/output `ENOSYS` classification. Unit
coverage crosses two upper-word encodings with three unusable outputs and five
input classes (30 rows, including real procfs). The real-KVM fixture crosses
the same encodings and outputs with four input classes (24 rows) for native,
direct KVM twice, and Tool KVM twice. Pipe/socket rows prove a distinct byte is
not consumed.

Exact-head gates:

- `cargo fmt --all -- --check`: green
- `cargo clippy -p reverie-kvm --all-targets -- -D warnings`: green
- new positive-output focused unit: 1/1 green, 818 filtered out
- negative-output focused unit: 1/1 green, 818 filtered out
- original low-word focused unit: 1/1 green, 818 filtered out
- required real-KVM fixture: 1/1 green, 341 filtered out; native once,
  direct KVM twice, Tool KVM twice; exact stdout/stderr/exit comparison
- full library parallel: 819/819 green in 8.94 s
- full library serial: 819/819 green in 29.15 s

Three earlier content-identical full-parallel attempts remain recorded red:

- 817/819: unrelated reserved-kick `Kvm(Error(4))` and positioned-I/O SIGPIPE
- 818/819: unrelated descriptor-retirement host `EAGAIN`
- 818/819: unrelated positioned-I/O SIGPIPE

They were not filtered or relabelled. A later pre-commit full parallel run and
the final exact-head run both passed 819/819; the exact-head serial run also
passed 819/819.

Mutation evidence:

- Restoring late output validation makes the first closed-output/pipe-input
  unit row return `ENOSYS` rather than `EBADF`, and the KVM guest exits 46.
- Checking only output existence while omitting early `ensure_writable` makes
  the first read-only-output/pipe-input unit row return `ENOSYS`, and the KVM
  guest exits 58.
- The earlier negative-output guard mutant remains killed by both unit and KVM
  tests; the existing successful high-word rows kill strict full-register fd
  decoding.

Three independent Codex reviews approved the exact full and incremental
artifacts with no correctness finding and no goalpost moving. A fresh external
Claude-family review of this committed head remains mandatory before merge.

The remote branch was fetched after push at the exact head above. Both changed
files were read back from the remote ref and matched the local files
byte-for-byte. The bot-authored PR body was also read back byte-for-byte, and
the exact-head evidence comment is
https://github.com/rrnewton/reverie/pull/628#issuecomment-5793376156.

Preserved limits:

- Non-null offset-pointer precedence is not made native. Ten reviewed rows were
  native `ESPIPE`, base `EBADF`, and former head `5d65b9f5` `ENOSYS`; this
  correction restores `EBADF` like base but does not claim native `ESPIPE`.
- Private synthetic fdinfo is intercepted before `sendfile` and can still
  return pre-dispatch `ENOSYS`.
- A valid writable pipe output still takes the existing `ENOSYS` mediated
  fallback; this slice does not add pipe zero-copy support.
- Existing `lseek` high-word-whence and captured-output ordering limits remain.

## Post-freeze read-only decoder census

At exact head `79f139bd7e1b8b789e1b770597c03b9f0bbd6050`, seven direct strict
descriptor conversions remain in `reverie-kvm/src/executor.rs`, covering nine
syscalls: `sendto`, `recvfrom`, `fstat`, `fstatfs`, `fchdir`, `getdents64`, and
the shared `fgetxattr`/`fsetxattr`/`fremovexattr` target validator.
`flistxattr` already consumes the low descriptor word.

One additional predispatch strict conversion in
`virtual_signalfd_write_error` affects `writev`, `pwrite64`, `sendto`, and
`sendmsg`. This makes `sendto` unsuitable for a mechanical decoder-only pair
with `recvfrom`: changing the guard also exposes a separate native pointer/type
ordering mismatch. Flags, address lengths, counts, signal selectors, and other
non-fd `try_from` sites are not part of this descriptor census.

The cleanest prospective slice is `fstat` plus `fstatfs`: two observation-only
decoder changes with no cwd, directory-cursor, or socket-queue mutation. A
future implementation should cover both positive upper-word encodings,
bit-31-invalid descriptors, valid aliases with bad output pointers, unchanged
output buffers, captured/proc policy controls, independent decoder mutants, and
native/direct-KVM/Tool-KVM output/exit/status parity. This is analysis only;
no next-slice source edit or slot was started while PR 628 remains open.

Native oracle `native-fstat-fstatfs-lowword.c` (SHA-256
`f967fdef6a41f9762eb8492539f76aa1b50a54d270b8763b69777ec77b521f6b`)
compiled with `-Wall -Wextra -Werror` and passed 100/100 repetitions. For both
`0x5a5a5a5a00000000 | fd` and `0xffffffff00000000 | fd`, `fstat` and `fstatfs`
succeeded; the same aliases with address 1 returned `EFAULT`. A low-word bit-31
descriptor, sign-extended `-1`, and a positive high-word alias of a closed fd
returned `EBADF` without changing sentinel-filled output. A closed alias plus
address 1 also returned `EBADF`, establishing fd-before-output-pointer ordering.

## Invalid external-review attempt

`run-claude-review.sh` invoked `claude -p` with the prompt on redirected stdin.
That is the known prompt-ingestion failure and is not review evidence. Its JSON
result is empty and stderr contains only the 93-byte launcher banner. Recorded
wrapper PIDs `3285734` and `3663379` no longer exist, and no live process matches
the exact wrapper path, prompt path, or `--output-format json` signature; no
signal was sent because there was no remaining process to target. Preserve
these artifacts only as failed-launch evidence. The coordinator-owned
positional-`-p` replacement review remains the sole external verdict source.

## PR 623 main rebase

Rebased the three bot-authored PR 628 commits from base
`7bc49f4c4d63018adba61d49246e847569518e33` onto verified current main
`96598dc07490ec15411845d27ed5bfd5c0a34076` after PR 623 landed. PR 623 changed
only `reverie-ptrace/src/lib.rs`, `reverie-ptrace/src/target_loader.rs`, and
`reverie-ptrace/src/target_loader/tests.rs`; the two PR 628 KVM files were
unchanged, so the rebase was conflict-free.

Rebased commit chain:

- `be9ba0113ee074c09f54b368c5c5dd405a74e419` — low-word sendfile/lseek decoding
- `4052142fafbcf4b5457bc6498dbdd2643429c318` — negative output ordering
- `748321a70995f1e7c1d5385de09f620f4becd685` — general output validation ordering

All three commits retain bot author and committer identity. The complete KVM
diff is byte-identical to the pre-rebase artifact:

- full base-to-head artifact: `sendfile-lseek-frozen-pr623-rebase.diff`
- full SHA-256: `af4e2e7f20c4262b3e0cc4eca4ada9382c55a2660da555bcca985564ca538d9c`
- correction artifact: `positive-output-ordering-pr623-rebase.diff`
- correction SHA-256: `30777bf3ee93ac725b48e10f55e68f0110713e5ace560feb6efa45beca5a4b41`

Exact-head gates after restoring every mutant:

- fmt and diff check: green
- all-target clippy with `-D warnings`: green
- three focused unit cells: 1/1 each, 818 filtered out
- required real-KVM cell: 1/1, 341 filtered out; native once, direct twice,
  Tool twice; exact stdout/stderr/exit comparison
- full library parallel: 819/819 in 7.46 s
- full library serial: 819/819 in 27.48 s

Mutation reruns after the rebase:

- strict full-register sendfile output decoder: unit failed `-9` versus 2;
  KVM guest exit 8
- strict full-register sendfile input decoder: unit failed `-9` versus 2;
  KVM guest exit 9
- strict full-register lseek decoder: unit failed `-9` versus 6;
  KVM guest exit 6
- clearing low-word bit 31 for sendfile output: unit returned `ENOSYS` instead
  of `EBADF`; KVM guest exit 34
- clearing low-word bit 31 for sendfile input: unit copied 1 byte instead of
  `EBADF`; KVM guest exit 4
- clearing low-word bit 31 for lseek: unit returned 0 instead of `EBADF`;
  KVM guest exit 5
- restoring late output validation: unit returned `ENOSYS` instead of `EBADF`;
  KVM guest exit 46
- omitting early writability validation: unit returned `ENOSYS` instead of
  `EBADF`; KVM guest exit 58

Removing only the explicit `out_fd < 0` branch is now an equivalent mutant:
the stronger general modeled/open-standard output validation still returns
`EBADF`, and both the focused unit and KVM cell remain green. It is not counted
as a killed mutant. Every tracked file was restored byte-for-byte before final
gates.

Three fresh internal adversarial reviews approved rebased head `748321a7` and
the artifact hashes above with no correctness finding or goalpost moving. The
old `79f139bd` external target is non-final. Do not merge until a fresh
exact-head Claude-family approval is relayed by the primary coordinator.

The rebased branch was force-updated only under an expected-tip lease for old
head `79f139bd7e1b8b789e1b770597c03b9f0bbd6050`. A post-push fetch resolved the
remote branch to `748321a70995f1e7c1d5385de09f620f4becd685`; both changed files,
the bot-authored PR body, and the evidence comment were read back byte-for-byte.
The rebased exact-head comment is
https://github.com/rrnewton/reverie/pull/628#issuecomment-5793828637.
