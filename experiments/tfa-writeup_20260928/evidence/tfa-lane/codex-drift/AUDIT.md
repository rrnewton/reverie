# Codex release-manifest drift audit (2026-09-25)

Host rollover observed on this devserver:
- launcher /usr/local/bin/codex_cli/codex: sha256 d22c52fa9e5f05fec73498dc730d54b9c7c7255f7d5cf941197369799ca18603 (was 71b8f5bd...)
- stable manifest codex.real: sha256 4b776a3e... -> `codex-cli 0.156.1` (version-stable-2.*, rc=0)
  This digest is byte-identical to the previously reviewed, blocked 0.156.1 `latest` manifest.
- latest manifest codex_latest.real: sha256 d291f08a... -> `codex-cli 0.156.1` (version-latest-2.*, rc=0)
  build_info.commit_hash = fbsource 6be63a60915d (2026-09-24 10:17 PDT), MSDK package 1815213503230005.

Fix status (owning lane: oncall devinfra_3p_ai, source third-party/codex/main/codex-rs):
- de089a75d6a1 (2026-09-25 10:26) "[Codex] Import latest source 0.157.0" (D121728319), upstream rust-v0.157.0 00c972ed.
- 18cdc3ab303f (2026-09-25 16:57) "[codex] Accept verified Btrfs mount device mismatches" (D121708813, Michael Sokolov,
  reviewed by joaonetto) - upstream issue openai/codex#47415.
- src/daemon_mounts-6be63a60915d.rs still "Reject[s] host mount aliases" and requires mountinfo device == st_dev;
  src/daemon_mounts-574ea00d02db.rs (master) masks ancestor aliases and accepts verified Btrfs mismatches.
  => Both installed releases were built before both fixes landed; they lack the ancestor-alias mask and the
     Btrfs subvolume device acceptance. They remain blocked with the unchanged socket-isolation blocker.
- No MSDK codex/codex_latest DotSlash build from a commit >= 18cdc3ab exists yet: master 574ea00d02db
  fbcode/3pai_tooling/codex/msdk/bin/codex_latest is built from 24281a08 (2026-09-25 01:18 PDT), before both
  de089a75 and 18cdc3ab; the host's latest is the older 6be63a60 build. Producing and rolling out that
  release is owned by devinfra_3p_ai's MSDK schedule; TFA must not mark any release eligible.
