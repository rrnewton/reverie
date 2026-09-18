# Fleet handoff: registered worktree inventory

Snapshot authority time: **2026-09-18T18:31:25Z**. A complete read beginning at that instant was byte-identical to the preceding complete read at 2026-09-18T18:30:41Z; both serialized inventories had SHA-256 `50b6763ea7f58547f84fd78474991b1e7fade9ad95a2d74c11b7105a96d37b2e`.

This artifact deliberately contains only quiescence-independent evidence. It contains no disk-reclaim estimate, filesystem-free-space delta, per-tree size, or whole-disk attribution. Those measurements require host-wide quiescence that this shared machine cannot promise.

## Method

Immediately before the snapshot, `origin` was refreshed successfully with `git fetch --prune origin` in dev-hermit, Hermit, and Reverie. Every row below came from `git worktree list --porcelain`. Presence was checked on the registered path, cleanliness with `git status --porcelain=v1 --untracked-files=all --ignore-submodules=none`, and reachability with `git for-each-ref --contains=<HEAD> refs/remotes/origin`.

“Reachable” means reachable from at least one refreshed remote-tracking ref, not necessarily `origin/main`. “SAFE metadata only” means the registered path is already absent and only prunable Git administration metadata remains. No present checkout is called safe without complete liveness evidence. No prune, deletion, cleanup, build, or registry mutation was performed.

## Totals

| Repository | Rows | Present | Absent | Clean | Dirty | Reachable rows | Unreachable rows | Unique unreachable HEADs |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| dev-hermit | 26 | 13 | 13 | 7 | 6 | 22 | 4 | 3 |
| hermit | 84 | 83 | 1 | 79 | 4 | 50 | 34 | 28 |
| reverie | 7 | 7 | 0 | 7 | 0 | 5 | 2 | 2 |
| **Total** | **117** | **103** | **14** | **93** | **10** | **77** | **40** | **33 repository-scoped** |

The Hermit result re-confirms the prior baseline: **34 rows across 28 unique HEADs are unreachable from every refreshed origin ref**. The refreshed/pruned Reverie namespace adds two clean but unreachable rows that the earlier snapshot did not expose.

Disposition summary:

- **14 SAFE metadata-only rows:** all are already absent; this does not authorize deleting any present directory.
- **42 UNSAFE rows:** present and dirty, present and unreachable, or both. Preserve them.
- **61 NOT DETERMINED rows:** present, clean, and reachable, but this inventory does not establish process/mount liveness. Preserve them.

## Canonical wrkslots authority

A read-only canonical `wrkslots audit --format json` at **2026-09-18T18:33:25Z** examined seven slot directories and returned **0 DELETABLE**. All seven were BLOCKED:

| Slot | Registration | Type | Why deletion is refused |
|---|---|---|---|
| `fix-agent-snapshot-spawning-cache` | unregistered | validate | no active registry row; ownership/liveness unverifiable |
| `fix-agent-snapshot-spawning-status` | unregistered | validate | no active registry row; ownership/liveness unverifiable |
| `hermit-904-tfa-layout` | unregistered | agent | no active registry row; ownership/liveness unverifiable |
| `hermit-909` | registered | agent | live identity observed; nested Git metadata in cache |
| `hermit-910` | registered | agent | live identity observed; owner generation indeterminate; nested Git metadata in cache |
| `pressure-471a14f9374e-20260906182318-ff468c29` | registered | validate | owner dead, but mount evidence exceeded the safety bound and is unverifiable |
| `validate-hermit-910-8c8` | registered | validate | live identity observed; owner generation indeterminate; nested Git metadata in cache |

The 14 SAFE rows below are stale Git worktree metadata whose paths are already absent; they are not active `wrkslots` rows. This distinction reconciles “14 SAFE metadata rows” with canonical `wrkslots` returning zero DELETABLE slots.

## Handoff

- Preserve every present checkout. The 42 UNSAFE rows contain dirty state or commits absent from refreshed origin refs.
- A future coordinator may prune only the 14 already-absent metadata rows after separately authorizing that registry maintenance; this audit did not mutate them.
- Re-evaluate the 61 NOT DETERMINED rows only with complete process, cgroup, mount, and handoff evidence. Clean plus published is not deletion authority.
- The Reverie primary row is reproduced exactly as Git reports it: `/home/newton/work/dev-hermit/.git/modules/reverie`; the working checkout used to invoke Git is `/home/newton/work/dev-hermit/reverie`.

## Per-row inventory

### dev-hermit

| Registered path | Presence | Git state | Refreshed origin reachability | HEAD | Disposition |
|---|---|---|---|---|---|
| `/home/newton/work/dev-hermit` | present | dirty | reachable (4 refs) | `bf0bf66df1c9357aae20448d32b41825c3b9cff6` | UNSAFE; retain |
| `/home/newton/dh137hub` | present | clean | reachable (240 refs) | `6cff4f3bc51dc9cd187546e39b025140b008bd62` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit-cihub-hermit-123` | present | clean | reachable (230 refs) | `7eb82518a9594d7c19ed1c4571c453234aff0a60` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/worktrees/122-030/dev-hermit-goal5-a` | present | dirty | reachable (224 refs) | `5e962584049bed79a238a62946dd9d1e545d66a7` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/worktrees/122-030/dev-hermit-goal5-b` | present | dirty | reachable (224 refs) | `5e962584049bed79a238a62946dd9d1e545d66a7` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/worktrees/130/devhub-pr2717-030` | present | clean | reachable (234 refs) | `9f879ee699e0468704b4dcfab81d562bd0aa4f61` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/worktrees/hermit-108/dev-hermit-current` | present | dirty | reachable (228 refs) | `37d4534d1b875e7c202a4104b4b8d76628375cea` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/worktrees/slots/hermit-904-tfa-layout` | present | clean | reachable (4 refs) | `1356352a5f132937422461546440e13706e7300b` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/worktrees/validate/fix-agent-snapshot-spawning-cache` | present | clean | reachable (23 refs) | `1357f5bafdb34e887208fa63d349f8ebe9243cb2` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/worktrees/validate/fix-agent-snapshot-spawning-status` | present | clean | reachable (1 refs) | `2ea532006937ec441fe2f497fc90c3ae88f78418` | NOT DETERMINED; retain |
| `/home/newton/work/devhub-h134-030` | present | dirty | reachable (236 refs) | `b89393233c5645628a5828d5f893acb631981351` | UNSAFE; retain |
| `/tmp/hermit-904-tfa-layout` | present | dirty | reachable (4 refs) | `d65467c892325c678b7a2944b38a77809ca0ffab` | UNSAFE; retain |
| `/tmp/hermit-906-policy-injection-fix` | absent | n/a (absent) | reachable (34 refs) | `9ba0bb1bd32779d02c82fd6c5bed7de792e1669d` | SAFE metadata only |
| `/tmp/hermit-906-wrkslots-provenance-fix` | absent | n/a (absent) | unreachable | `28b7c2d7134a5a605811c9a58caf98f8fb6fac35` | SAFE metadata only |
| `/tmp/hermit-909-parent-lsof` | absent | n/a (absent) | reachable (31 refs) | `25f209863c246095006ce3a3ca886d425bb67832` | SAFE metadata only |
| `/tmp/hermit-909-review-354-old` | absent | n/a (absent) | reachable (23 refs) | `09f1b5d3113238a4b55be54855e83e843cba81c7` | SAFE metadata only |
| `/tmp/hermit-909-review-355` | absent | n/a (absent) | unreachable | `932e4fb88490b0bc37552d6c417581e519dab158` | SAFE metadata only |
| `/tmp/hermit-909-review-355-base` | absent | n/a (absent) | reachable (23 refs) | `1357f5bafdb34e887208fa63d349f8ebe9243cb2` | SAFE metadata only |
| `/tmp/hermit-909-review-355-mutant` | absent | n/a (absent) | unreachable | `932e4fb88490b0bc37552d6c417581e519dab158` | SAFE metadata only |
| `/tmp/hermit-909-spool-path` | absent | n/a (absent) | reachable (31 refs) | `e8c51e064dfb51d7629872fdf253a4bd67beb729` | SAFE metadata only |
| `/tmp/hermit-910-parent-land-8c8` | absent | n/a (absent) | reachable (32 refs) | `08daf991e175602c9251ea733cb59510f5e1fe4a` | SAFE metadata only |
| `/tmp/hermit-910-partial-pack-fix` | absent | n/a (absent) | reachable (33 refs) | `26cd4a8b9161ced400df703a7b6472c8df818061` | SAFE metadata only |
| `/tmp/hermit-910-reverie-pin` | absent | n/a (absent) | reachable (33 refs) | `5811a664913168247513b7b47700b7af5157ab4b` | SAFE metadata only |
| `/tmp/hermit-910-reverie-pin-8c8` | absent | n/a (absent) | unreachable | `f29ce4bbb85ba456beb64e6cde5f9e19fc34fc02` | SAFE metadata only |
| `/tmp/hermit-910-toolroot` | absent | n/a (absent) | reachable (32 refs) | `21a1a005d0183e69d5977358a7fbc20032678182` | SAFE metadata only |
| `/tmp/tfa-push-candidate-artifact-906` | present | clean | reachable (4 refs) | `ee6562142e3796bb56a1dd0e28b77c1c16a78cc2` | NOT DETERMINED; retain |

### hermit

| Registered path | Presence | Git state | Refreshed origin reachability | HEAD | Disposition |
|---|---|---|---|---|---|
| `/home/newton/work/dev-hermit/hermit` | present | clean | reachable (3 refs) | `fb0a4e3f39742f90cd8e69668d9f52c7d870e901` | NOT DETERMINED; retain |
| `/home/newton/temp/hermit-mg` | present | clean | unreachable | `233c0f144fefa8be431bab0ccf6c2a5a7755555f` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-2KBchq2d` | present | clean | unreachable | `fcf99cef9dea7d0fc7a578690980112141ce3501` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-2S0jiF0O` | present | clean | unreachable | `3a26b49ce113a095c626aa878b43ff8b8f551d8c` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-2fdNowW9` | present | clean | reachable (1174 refs) | `72a837868d55ed5c4b8d79cc5070e63bbd3b25e1` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-4fCi4OOI` | present | clean | reachable (1179 refs) | `b132f72c6c411f441ae99314a1e4d32f56b4bc7f` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-5roX1FVh` | present | clean | unreachable | `75f7e25bb8ccddf3843861f06b74009b14ad374c` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-6SObwrsl` | present | clean | reachable (1 refs) | `a04f0eadd909c485ff767667b028a1dfc7d3e352` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-6svv8DFQ` | present | clean | unreachable | `81692e6b72e06ccc8d5a1d1c5647cbfe4929027a` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-7BWs2tlg` | present | clean | reachable (1 refs) | `abe1d19ad59cf4d825592cb59cfe5044485b7b30` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-8MCdD0Aq` | present | clean | reachable (1 refs) | `d4384648c8d6b40e37097c53e9ccaa1f1e2dab84` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-8Pzi3Lx8` | present | clean | unreachable | `75f7e25bb8ccddf3843861f06b74009b14ad374c` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-93SoTsqi` | present | clean | reachable (1 refs) | `a04f0eadd909c485ff767667b028a1dfc7d3e352` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-9KKdvInQ` | present | clean | reachable (1 refs) | `6f463f24f2b796cc8481a22a6a5d88219366f9ab` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-C7DVP6JA` | present | clean | reachable (1179 refs) | `b132f72c6c411f441ae99314a1e4d32f56b4bc7f` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-D3zWX3zz` | present | clean | reachable (989 refs) | `1f4e430c135204591daa0c0cba84f77dae679a78` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-DFQF8F5v` | present | clean | unreachable | `33b54776296620300f1250f9ab36695dcce73ab1` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-G01s9K6N` | present | dirty | reachable (243 refs) | `1ce198d2f224240ea7e648e92a10e6e8d7f91a93` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-Hwa8Zmai` | present | clean | unreachable | `6075a607fa532c37fe34b481ef43295d48ff6a35` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-JvxHDMHf` | present | clean | reachable (1 refs) | `be651570b6097e1461167bc443f5d60e49a0a77b` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-LZVTDUCY` | present | clean | unreachable | `367fa13859ef4d6a74002300c0156be8d899e5b3` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-LuSMtTtr` | present | clean | unreachable | `ee33a4412210ce62e3e006bd4e172d72796eb223` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-Lvl9eDZs` | present | clean | unreachable | `233c0f144fefa8be431bab0ccf6c2a5a7755555f` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-MAtPqb6Q` | present | clean | unreachable | `97bd502435b42d0416378e4363e7432563a48047` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-NtrvWMXX` | present | clean | reachable (1 refs) | `a04f0eadd909c485ff767667b028a1dfc7d3e352` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-OE4PLSNW` | present | clean | reachable (1 refs) | `abe1d19ad59cf4d825592cb59cfe5044485b7b30` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-SgbZKaJJ` | present | clean | unreachable | `202b0c3f3bc97d0d1d4de2305532a7663d7759d7` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-TdvM1fjo` | present | clean | reachable (1 refs) | `127df1a5c35a0f4261f414fa3d19d7e8e4776a3d` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-V3oS5ufb` | present | clean | reachable (1 refs) | `ba360ff3c5208a441af6123735a41e26caf149f9` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-V4wBbxs1` | present | clean | reachable (1 refs) | `a3f7f1a32bd57994245be8fa1ab2877ef86724cf` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-WyedcRk2` | present | clean | unreachable | `97bd502435b42d0416378e4363e7432563a48047` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-aLx9ThTN` | present | clean | unreachable | `87ada3efc5e36afe7a920b61115dbea07cf59c2e` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-bf6JHMA4` | present | clean | reachable (1097 refs) | `d515b698cff4c88a7a0fa6f90695ed1570c61d30` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-dAcBzS9h` | present | clean | unreachable | `4da2c30ca7222851be8ebdbceac8d0e93b9d328d` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-jdJXphik` | present | clean | reachable (1 refs) | `a7a4bb90e2ddb8363414faacfa43a8ccfbe50409` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-kGzTwprL` | present | clean | unreachable | `bcd9b7759dc90e1fd048c736f11a32bfa238e5aa` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-l0jsGLQt` | present | clean | reachable (1 refs) | `e323cdb244c76630965b46726f7d2bb9d617be39` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-lSuTWFjY` | present | clean | reachable (1 refs) | `cc9f69552a02c107dc3acf912cd49242a67c6d98` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-nicx25xx` | present | clean | unreachable | `27fdb69928f2b51ad26fc2ed1d2afea0ba624054` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-sPt8v2Sx` | present | clean | reachable (1181 refs) | `d21bce4e90416dab44880cc7949a8a54d2451d90` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-sR7J0ypP` | present | clean | reachable (1 refs) | `66138d34910d325db9c4a95d265aa06a0a4f6aab` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-uWZLug3X` | present | clean | reachable (942 refs) | `e2f9a36b1ad80abd0c012f6ca5082b0613ff2cad` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-x5S7J4f9` | present | clean | unreachable | `3a26b49ce113a095c626aa878b43ff8b8f551d8c` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-xumfMeRD` | present | clean | reachable (1 refs) | `e323cdb244c76630965b46726f7d2bb9d617be39` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/ignored/validate-fresh-yRttK1Z2` | present | clean | reachable (907 refs) | `302a1a9c0fde564db0292d1ea5cabc91343e79bc` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/ignored/wt-liteinst-cells` | present | clean | unreachable | `fa9df6bca44b1c9395d6f8d759a2a352aaecad0a` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/ignored/wt-trend-16d47dfeb` | present | clean | reachable (1014 refs) | `16d47dfebcffdf73ff800a5cd8a71ef84b7d4a5b` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/ignored/wt-trend-f62f403e6` | present | clean | reachable (1097 refs) | `f62f403e63dc841352f01a6c657f2906362f561d` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/ignored/wt-xrec` | present | clean | reachable (1 refs) | `754942211294ca62eb64e5f2bf393234c7516045` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/worktrees/106-baseline/hermit` | present | clean | reachable (243 refs) | `1ce198d2f224240ea7e648e92a10e6e8d7f91a93` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/worktrees/122-030/hermit-main-1ce198d2` | present | clean | reachable (243 refs) | `1ce198d2f224240ea7e648e92a10e6e8d7f91a93` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/worktrees/122-030/hermit-resource-caps-v7` | present | clean | reachable (1 refs) | `56ec0172b513602bb7bc4c9a9fa5c7dbf22cdcb7` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/worktrees/130/hermit-pr2717-devbig030` | present | clean | reachable (1 refs) | `89f6eaa95f20ddd3459ff680b59486b769099c13` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/worktrees/137/hermit` | present | clean | reachable (319 refs) | `a6b0c37648df774d5859aad23a535caf03a6d392` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/worktrees/138/hermit` | present | clean | unreachable | `fc311ca956bddec4c6339f6a9f2eba246baa73a3` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/worktrees/agg2129/hermit` | present | clean | reachable (1 refs) | `67c30b06da5949f05759124a815ebdebcf548992` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/worktrees/b2fix/hermit` | present | clean | unreachable | `fcdba847dfb15934fc37767f8efd9347211225ef` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/worktrees/claude-1/hermit` | present | clean | unreachable | `27fdb69928f2b51ad26fc2ed1d2afea0ba624054` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/worktrees/claude-exitgate/hermit` | present | clean | reachable (1 refs) | `d857b6b853ad587f2ab9fbd261c49e8ed61463ba` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/worktrees/codex-hostsweep907/hermit` | present | clean | reachable (989 refs) | `1f4e430c135204591daa0c0cba84f77dae679a78` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/worktrees/codex-lsof908/hermit` | present | clean | reachable (1 refs) | `fd0d6b3734ddfe1c526732c7c63b4860e95bf801` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/worktrees/codex-robust904/hermit` | present | clean | reachable (1 refs) | `7f3da389906266b043695e0be0516a74fb7daa4b` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/worktrees/coord/hermit` | present | clean | reachable (1 refs) | `1ab82aa6dcf4fc73a297f0dfca5adbe36777be28` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/worktrees/fixture-align/hermit` | present | clean | unreachable | `a6df197cf8ce9954c492812b796d577aa99bf6f3` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/worktrees/gchat-crash-902/hermit` | present | clean | reachable (806 refs) | `bec7551586ae449658b0320ba1374186dda82bb5` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/worktrees/h137val2` | present | clean | reachable (319 refs) | `a6b0c37648df774d5859aad23a535caf03a6d392` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/worktrees/hermit-108/hermit` | present | clean | reachable (248 refs) | `63b778646101bb7228a1fc1674c937274b75aab4` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/worktrees/hermit-123-pr2637/hermit` | present | clean | unreachable | `95c5adeede661ec73b59ddd32f7f221e36ccbdb5` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/worktrees/hermit-127-pr2674` | present | clean | unreachable | `54d73ed1c81fe2dd43189fb2b3b613a55a677d97` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/worktrees/hermit-909-reverie-pin-hermit` | present | clean | unreachable | `29d3130aff48a4c13778da84d4ec1b9763dbbf4d` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/worktrees/lander/hermit` | present | clean | reachable (1 refs) | `6a626da4b96b194957e1494fe7c1677182078d7c` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/worktrees/n2fix/hermit` | present | clean | unreachable | `c7be593d1a802bd7154078e2d4d88625e9500bfb` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/worktrees/recresearch/hermit` | present | clean | reachable (15 refs) | `7d55fcd6441dbaacf7026e2005ac1468d80fb2d5` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/worktrees/review1/hermit` | present | clean | unreachable | `4806f6d210763b54433792075e84e34e7b18aa76` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/worktrees/scorecard/hermit` | present | clean | reachable (1 refs) | `e6c4fe996ae480ee5108d5e8d136839451d00176` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/worktrees/shebang/hermit` | present | clean | unreachable | `47692a3a459aba7facaf85026070ddd12511728d` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/worktrees/slot05/hermit` | present | clean | reachable (989 refs) | `1f4e430c135204591daa0c0cba84f77dae679a78` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/worktrees/slot10/hermit` | present | clean | reachable (1 refs) | `27526f68f4d7ce526a6f5f7749d28c2252aee629` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/worktrees/slots/hermit-909` | present | dirty | unreachable | `991174b029b7710f49c1f637901752a5a6252557` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/worktrees/slots/hermit-910` | present | dirty | unreachable | `e8890afc7c67df984d86af9fdbeeb7b79c083b1f` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/worktrees/validate/pressure-471a14f9374e-20260906182318-ff468c29` | present | clean | reachable (50 refs) | `471a14f9374e7d4c05c96a6da18ff7c5e4de64b5` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/worktrees/validate/validate-hermit-910-8c8` | present | dirty | unreachable | `a2d55575db78a23e2e466542a1c525e0f503ae45` | UNSAFE; retain |
| `/home/newton/work/h134-c1b8-030` | present | clean | reachable (319 refs) | `c1b8c5db9c1b518c2a0598676b9025cc41ddcc5a` | NOT DETERMINED; retain |
| `/tmp/hermit-909-hermit-pin` | absent | n/a (absent) | unreachable | `991174b029b7710f49c1f637901752a5a6252557` | SAFE metadata only |

### reverie

| Registered path | Presence | Git state | Refreshed origin reachability | HEAD | Disposition |
|---|---|---|---|---|---|
| `/home/newton/work/dev-hermit/.git/modules/reverie` | present | clean | reachable (2 refs) | `44fcb1955f44547f50d724fe8f7d718215fed446` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/ignored/kvm2-cherrypick-test` | present | clean | reachable (1 refs) | `5bd1b852ad2c649a5fd0c11f261053cbd2be903c` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/scratch/reverie-kvm17` | present | clean | reachable (1 refs) | `053372ad9fb8176153a54b9cc7df22e1dc45ef06` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/worktrees/claude-1/reverie` | present | clean | reachable (197 refs) | `99437f05e82377a80ad1edb9e501d89a38c91ecb` | NOT DETERMINED; retain |
| `/home/newton/work/dev-hermit/worktrees/coord/reverie` | present | clean | unreachable | `6ad260c12eeabf9574fa90f05c8e967789d27796` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/worktrees/lander/reverie` | present | clean | unreachable | `bea5abfca1220a893444db5e1056a902adfe07ef` | UNSAFE; retain |
| `/home/newton/work/dev-hermit/worktrees/scorecard/reverie` | present | clean | reachable (1 refs) | `d2216e8de8c0dfa8c617cb8884f971b844015196` | NOT DETERMINED; retain |
