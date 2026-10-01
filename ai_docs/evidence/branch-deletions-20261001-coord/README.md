# Deletion record: hermit branches coord/cpuid-3460 and coord/hosted-release-producer-3458

Background and terms:
- These are two hermit branches the coordinator pushed so it could land fixes on hermit main, which is fast-forward only. A branch like this has no purpose once its content is on main.
- "Patch-id" is `git patch-id --stable` of a commit's diff. Two commits with the same patch-id make the same change, even if their SHAs differ because one was rebased.

The two branches:
- `coord/cpuid-3460` at 01341d66633bd2da09ce932b05f623590a1e0500 ("Do not subscribe to cpuid when CPUID virtualization is off"). That exact commit is hermit main: it landed as a fast-forward at about 13:10Z on 2026-10-01, after local validate RUN 1989 passed. It fixes https://github.com/rrnewton/hermit/issues/3460. Hosted run https://github.com/rrnewton/hermit/actions/runs/36867662431 on that main passed the test that issue was about (`log_diff_does_not_count_the_epoch_notice_as_evidence`).
- `coord/hosted-release-producer-3458` at 0302849bf0991317d3de5eace2a7f3f73a12aa6b. It is hermit main 04439cbb plus two commits. Both are on main as rebased commits with the same patch-id:
  - e310dcd105ee17608f6fb98673697072b5d824ad "Publish the hosted E2E artifact from the one validate-profile build": patch-id 6330d3db, on main as 8802a55dcaaf2f48315b2909df59a513a4e18197.
  - 0302849bf0991317d3de5eace2a7f3f73a12aa6b "Pin the counted input gates and the symlink loop in the shard guard": patch-id 2b2503cb, on main as 6cdb1a2cecbafd4ed611daad0ed10f1c1d9d206f.
  - Together they fix https://github.com/rrnewton/hermit/issues/3458. In run 36867662431, "Publish E2E artifact" passed and the E2E shards ran.

Each branch is deleted with an expected-SHA lease on the tip above, so the deletion fails if the branch has moved:

    git push --force-with-lease=refs/heads/coord/cpuid-3460:01341d66633bd2da09ce932b05f623590a1e0500 origin :refs/heads/coord/cpuid-3460
    git push --force-with-lease=refs/heads/coord/hosted-release-producer-3458:0302849bf0991317d3de5eace2a7f3f73a12aa6b origin :refs/heads/coord/hosted-release-producer-3458

Recorded by: coord (claude, devbig014), 2026-10-01T14:22Z.
