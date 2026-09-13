# Isolate the received-rights closure test

The immediate EOF assertion in `received_rights_reservation_and_rewrite_failures_are_transactional` can fail when another library test creates a host subprocess between socket creation and rollback. CLOEXEC sockets are inherited until that subprocess execs. The rollback can close its descriptor correctly while the subprocess still owns the same socket object.

The test now invokes its unchanged fixture through an exact-test subprocess, using the existing bounded subprocess pattern in `pipe_fionread_capture_objects_and_unavailable_identity_are_refused`. Socket creation happens after this subprocess execs, so other tests in the original library process cannot inherit those endpoints. The child has a ten-second deadline and two-second kill grace; any child error includes its status, stdout and stderr and fails the parent test.

The only code change relative to `7061a4cc16318417988690f39cadbb049874ed2b` is a 23-line insertion at the start of the existing test. The complete old fixture, immediate EOF helper and both call sites remain byte-identical. No runtime implementation, scheduler policy, core API, assertion or retry behavior changed. The library inventory remains exactly 375 tests with identical names.

## Causal evidence

The original full-run failure remains recorded: 374 library tests passed and this test failed at executor.rs:13678, with recv(MSG_DONTWAIT) returning -1/EAGAIN instead of EOF/0. The complete command took 12.253 seconds including compilation. The original panic does not distinguish the rollback call from the later unsupported-ancillary close call, and its historical process interleaving is unknown.

A bounded ptrace diagnostic runs only this test and the existing isolated SIGPIPE subprocess test from an unchanged copied library binary. It changes process scheduling only; it does not edit registers, syscall results or assertions.

| Source and controlled child schedule | Original library result | Elapsed |
| --- | --- | --- |
| Preserved c53, hold actual subprocess before exec across rollback | 101, original EAGAIN/EOF assertion | 0.230 s |
| Base 7061, same hold | 101, original EAGAIN/EOF assertion | 0.233 s |
| Base 7061, allow child exec before rollback | 0, both tests pass | 0.226 s |
| Isolated fixture, hold unrelated original-process child before exec | 0, both tests pass | 0.436 s |
| Isolated fixture, allow that child exec first | 0, both tests pass | 0.441 s |

The base trace records clone3 flags 16640 (CLONE_VM and CLONE_VFORK, without CLONE_FILES), socket inode 1068754427 and successful parent close(13). The parent descriptor is absent afterward, while the stopped subprocess still owns that exact socket inode. The peer recv then returns -11. In the isolated candidate, the fixture's endpoints belong to its separate executed process; the held unrelated child cannot name those socket objects and both immediate EOF assertions pass.

This demonstrates a preexisting process-isolation defect and a correction for that mechanism. It does not retrospectively identify the original full run's subprocess. The first diagnostic setup mishandled child-stop/clone-event ordering and hit its twelve-second deadline; that failed setup and its source remain retained, separate from the successful controls. No time bound was widened to correct it.

## Oracle and validation

A temporary test-only mutation retains an extra cloned endpoint through the first EOF assertion. The isolated test still fails with the original -1/EAGAIN versus 0 assertion, and its subprocess error propagates to the parent test. The mutation returns 101 in 3.212 seconds including compilation. The mutation is not in the candidate, and restored source bytes were verified before final checks.

With `REVERIE_REQUIRE_KVM=1`, the focused test passes in 3.805 seconds including its build. The complete concurrent KVM library suite passes all 375 tests, zero ignored or failed, in 11.122 seconds including compilation (7.95 seconds in libtest). Clippy for all KVM targets with warnings denied passes in 2.306 seconds; format checking passes in 1.286 seconds. This test-only change does not rerun or newly qualify the separate Hermit vector/signal composition; its retained full comparison failure remains a failure.

Exact source and copied binaries:

- Candidate executor.rs SHA256: bcc8196cedd3b35a3fcba0ffc750e3d247a5cc907d6be7e47ab51a9f09e66a4c.
- Candidate copied library SHA256: 40a99bf9338c1b19553820971c303fae874a6da746d143ccf2b0edf18dac39b5.
- Base 7061 copied library SHA256: fccda97a23f683cd00dda9c4b02ddf3804e0c00a544f8c6705c98047e5a1db49.
- Preserved c53 copied library SHA256: aee428158cf9a09af9838d77b307dda7364677ebe1db0381202c4e01d0206aed.
- Extra-endpoint mutation copied library SHA256: 57d61b99e20ea4d5f604316379702dea13decaeee8db729574369f3cf70e2a12.

Evidence paths in the implementation workspace:

- Complete source trace and original/failing control provenance: `/tmp/astra-reverie-scm-rights-causal-review.md`, verified in TaskGraph on `astra-reverie`.
- Original current/base scheduling controls: `/tmp/astra-reverie-scm-rights-inheritance-control.py` and `/tmp/astra-reverie-scm-rights-inheritance-c53-control.py`.
- Isolated-process scheduling control: `/tmp/astra-reverie-scm-rights-isolation-control.py`.
- Exact insertion and unchanged inventory proof: `/tmp/astra-reverie-scm-rights-isolation-preservation.json`.
- Mutation patch, source-restoration proof and copied binary: `/tmp/astra-reverie-scm-rights-leak-mutation.patch` and `/tmp/astra-reverie-scm-rights-leak-mutation.json`.
- Actual command/environment/status logs: `/tmp/astra-reverie-terminal-controls/scm-isolation-{focused,library,clippy,fmt,leak-mutation}.{json,stdout,stderr}`.
- File hashes for this component: `/tmp/astra-reverie-scm-rights-isolation-artifacts.json`.

No assertion was weakened, tolerance widened, case skipped, comparator relaxed, failure relabelled as a pass or check deleted. The correction isolates the fixture's descriptor ownership so the original immediate EOF assertion measures the intended close operation.
