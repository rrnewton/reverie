# Unit gate at ef4885b in /home/newton/work/tfa/fbsource: 131/132 (rc=32), 2026-09-25T06:59:20Z-06:59:28Z

Failing test: testinfra.tfa.tests.test_prototype.ProcessTest.test_real_packaged_hermit_is_bound_to_blocked_provenance
(test_prototype.py:1201). It expects "packaged Hermit compatibility is BLOCKED" and got
"packaged Hermit compatibility is PROVENANCE_MISMATCH". This is a stricter refusal from the same function,
hermit.py `packaged_hermit_argv`: `packaged_sha256 != packaged["sha256"]` is checked before the BLOCKED arm.

Cause, measured:
- hermit-import-compatibility.v1.json pins packaged_executable.sha256 = 9bf7064a98556c08...
  (the same value as PACKAGED_HERMIT_RESOURCE_SHA256 in hermit.py). Neither file is changed by ef4885b.
- The resource this checkout built (hermit-test-bin -> buck-out/v2/art/fbcode/512fd0a9ea51f545/.../hermit,
  built 2026-09-24T22:48:51-07:00 during this venue's first production run) has sha256 4e8e438148c77fb3...
- Its `fb_build_info` ELF section (saved as hermit-test-bin.fb_build_info.bin) stamps
  "revision":"ef4885b6...", "upstream_revision":"4c52f205...", "epochtime":1790315331,
  "time_iso8601":"2026-09-24T22:48:51Z", "path":"/home/newton/work/tfa/fbsource", "host", "user".
  The target is built with `fbcode-build-info-mode[full]` (testinfra/tfa/BUCK hermit-test-bin-opt).
  Every fresh link therefore gives a new sha: another checkout, another revision (including
  a message-only amend) or another build time. The pinned sha only reproduces through a
  buck-out/action-cache hit on the original link.
- The earlier 132/132 (receipts/unit-68cf2bac-recheck.out, run 13510799084166763) ran in the old /tmp
  checkout, whose buck-out presumably still held that original link. The handoff's "tree content identical,
  so receipts carry" argument does not hold for this test, because its input bytes depend on more than tree
  content.

Classification: an existing reproducibility defect in a pin from before this commit. It fails CLOSED, so no
gate is weakened. It is not caused by the Codex work. I did not re-pin the sha, normalise the hash, or
change the build-info mode: each of those rewrites a provenance claim whose blockers were observed on
9bf7064a, so that is an owner/review decision. Other gates at the same time: arc pyre rc=0 (3 targets, no type
errors); validate-manifest rc=0 {"schema_version": 1, "cases": 3}; `sl status` clean.
