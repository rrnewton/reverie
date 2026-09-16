# Legacy vsyscall finding: scope clarification

The frozen measurement establishes a legacy Linux service entry that returns
successfully without the installed SUD controller's callback on this host.
It does **not** establish an ordinary native64 syscall-instruction site that
requires patching. It also does not establish patching as a unique, sufficient,
or viable remedy for the legacy service bypass.

No seccomp trap layer or other interception mechanism may be silently added and
reported as SUD-only. The retained experiment's inherited ptrace-denial filter
only rejects ptrace; it does not intercept these services or fix the observed
bypass. Any proposed additional mechanism requires explicit architectural
classification, owner authorization and separate evidence.

Original report and all source, binaries, commands, observations and hashes stay
unchanged. Jason's independent review/reproduction is pending. No new experiment,
implementation, production activation or remedy claim follows from this note.

Original report: `SUD-VSYSCALL-EVIDENCE.md`.
Frozen evidence: `/home/newton/work/dev-hermit/ignored/liteinst-vsyscall-sud-20260904/`.
Product remains at `8e8d36adf2ea220c35a5d2bc0c6a41aa90f92675`.
