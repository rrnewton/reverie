# Evidence copies: manifest

Copied 2026-09-28 by sub-tfa2-030 (integrator) on devbig030, so that the non-repository files cited by
[../README.md](../README.md), [../HERMIT_DEFECTS.md](../HERMIT_DEFECTS.md), [../TARGETS.md](../TARGETS.md),
[../EXPERIMENTS.md](../EXPERIMENTS.md), [../STACK_STATE.md](../STACK_STATE.md) and [../WAY_FORWARD.md](../WAY_FORWARD.md)
survive `/tmp` cleanup and slot reclamation. Each file was checked by pattern search for credentials and signed links
before copying; no binaries were copied. Totals: 226 files, 1,481,926 bytes; every file is under 200 KB. The last 7
(the six `verify/` files and `tfa-lane/codex-rollover/testplan.txt`) were added by the final completeness pass, so that
the verify pass's re-queries cited in the documents are also durable.

The gatherer notes (`gather-*.md`) are verbatim, so paths inside them still name the originals. Translate a
cited original path to its copy with these rules, or look it up in the table:

| Original path | Copy |
|---|---|
| `/tmp/sub-tfa2-030/gather/<key>.md` | `gather-<key>.md` |
| other files under `/tmp/sub-tfa2-030/gather/` (selected) | `gather-raw/<same relative path>` |
| `/tmp/sub-tfa2-030/ti/<file>`, `/tmp/sub-tfa2-030/ti-timerfd.json` | `ti/<file>`, `ti-timerfd.json` |
| `/tmp/sub-tfa2-030/gh-*.json`, `/tmp/sub-tfa2-030/way/<file>` | `gh/gh-*.json`, `way/<file>` |
| `/tmp/sub-tfa2-030/verify/<file>` (6 selected verify-pass outputs) | `verify/<file>` |
| `/tmp/sub-tfa2-030/devvm895/...` (4 files, renamed; see table) | `devvm895/<name>` |
| `/home/newton/work/tfa/<subdir>/<file>` (selected) | `tfa-lane/<subdir>/<file>` |
| `/tmp/tfa-phase2/<path>` and `/tmp/tfa-phase2-<name>` (selected) | `tmp-tfa-phase2/<path>` and `tmp-tfa-phase2/tfa-phase2-<name>` |
| `worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/RESULTS.md` (dev-hermit, untracked) | `phase2-three-case-RESULTS.md` |

**Renames.** Every copied `*.log` file is stored as `*.log.txt`, because dev-hermit's `.gitignore` (line 94,
`*.log`) would otherwise make `git add` skip it silently. Content is unchanged.

**Redactions.** In the three files marked "yes" below, the `signature=` value of an expired signed
`fb-vscode://nuclide.code-compose/devmate/fixtests` link was replaced with `REDACTED-BY-INTEGRATOR`
(`sed -E 's/signature=[0-9a-fA-F]+/signature=REDACTED-BY-INTEGRATOR/g'`). Nothing else changed, so line
numbers match the originals. For every other file the copy's SHA-256 equals the original's.

## Copied files

| Copy | Original | Bytes | SHA-256 of original (first 16 hex) | Redacted |
|---|---|---:|---|---|
| [devvm895/STACK_SUMMARIES.md](devvm895/STACK_SUMMARIES.md) | `/tmp/sub-tfa2-030/devvm895/tfa-src/STACK_SUMMARIES.md` | 62,572 | `859dbe3a91c75562` | no |
| [devvm895/containment-local.log.txt](devvm895/containment-local.log.txt) | `/tmp/sub-tfa2-030/devvm895/tfa-diag/containment-local.log` | 33,439 | `10cf33ed75a857a3` | yes |
| [devvm895/previous-way-forward-draft.md](devvm895/previous-way-forward-draft.md) | `/tmp/sub-tfa2-030/devvm895/tfa-diag/way-forward.md` | 8,143 | `c6d8090bd2b34680` | no |
| [devvm895/previous-way-forward-research.json](devvm895/previous-way-forward-research.json) | `/tmp/sub-tfa2-030/devvm895/tfa-diag/way-forward-full.json` | 108,593 | `9c20071bcce229fe` | no |
| [gather-campaign.md](gather-campaign.md) | `/tmp/sub-tfa2-030/gather/campaign.md` | 46,563 | `1b4d67ee6f403b88` | no |
| [gather-continuations.md](gather-continuations.md) | `/tmp/sub-tfa2-030/gather/continuations.md` | 37,448 | `5d4e8c02d4a49307` | no |
| [gather-fixability.md](gather-fixability.md) | `/tmp/sub-tfa2-030/gather/fixability.md` | 50,443 | `22649afbcda2fec9` | no |
| [gather-phase2.md](gather-phase2.md) | `/tmp/sub-tfa2-030/gather/phase2.md` | 51,433 | `7d0d06f8b586bf81` | no |
| [gather-raw/batch-describe.json](gather-raw/batch-describe.json) | `/tmp/sub-tfa2-030/gather/batch-describe.json` | 111,444 | `73a0055321fc55f9` | no |
| [gather-raw/campaign_recount.py](gather-raw/campaign_recount.py) | `/tmp/sub-tfa2-030/gather/campaign_recount.py` | 17,556 | `c14e54266646a281` | no |
| [gather-raw/campaign_verify_classify.out](gather-raw/campaign_verify_classify.out) | `/tmp/sub-tfa2-030/gather/campaign_verify_classify.out` | 11,966 | `90789d6393238f0c` | no |
| [gather-raw/campaign_verify_classify.py](gather-raw/campaign_verify_classify.py) | `/tmp/sub-tfa2-030/gather/campaign_verify_classify.py` | 2,772 | `628d170eb3301284` | no |
| [gather-raw/stackstate-raw/cistate-D121244213.json](gather-raw/stackstate-raw/cistate-D121244213.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/cistate-D121244213.json` | 480 | `87d875071b2e228b` | no |
| [gather-raw/stackstate-raw/cistate-D121244476.json](gather-raw/stackstate-raw/cistate-D121244476.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/cistate-D121244476.json` | 481 | `eabd713dcaee308a` | no |
| [gather-raw/stackstate-raw/cistate-D121649569.json](gather-raw/stackstate-raw/cistate-D121649569.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/cistate-D121649569.json` | 482 | `57bef8604451b6bb` | no |
| [gather-raw/stackstate-raw/cistate-D121848525.json](gather-raw/stackstate-raw/cistate-D121848525.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/cistate-D121848525.json` | 482 | `8688e881bf6de8ad` | no |
| [gather-raw/stackstate-raw/cistate-D121848885.json](gather-raw/stackstate-raw/cistate-D121848885.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/cistate-D121848885.json` | 482 | `9541bd5f2e56783a` | no |
| [gather-raw/stackstate-raw/cistate-D121848886.json](gather-raw/stackstate-raw/cistate-D121848886.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/cistate-D121848886.json` | 480 | `c79bfe32f5bdd019` | no |
| [gather-raw/stackstate-raw/cistate-D121848888.json](gather-raw/stackstate-raw/cistate-D121848888.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/cistate-D121848888.json` | 482 | `535060cedbd7135b` | no |
| [gather-raw/stackstate-raw/cistate-D121848889.json](gather-raw/stackstate-raw/cistate-D121848889.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/cistate-D121848889.json` | 482 | `4a32fe99953f83f6` | no |
| [gather-raw/stackstate-raw/cistate-D121848890.json](gather-raw/stackstate-raw/cistate-D121848890.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/cistate-D121848890.json` | 482 | `54d5e649a06a29f0` | no |
| [gather-raw/stackstate-raw/cistate-D121848892.json](gather-raw/stackstate-raw/cistate-D121848892.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/cistate-D121848892.json` | 481 | `2d022fbe32b58f30` | no |
| [gather-raw/stackstate-raw/cistate-D121848893.json](gather-raw/stackstate-raw/cistate-D121848893.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/cistate-D121848893.json` | 482 | `71281807456bbebe` | no |
| [gather-raw/stackstate-raw/cistate-D121848894.json](gather-raw/stackstate-raw/cistate-D121848894.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/cistate-D121848894.json` | 481 | `c39e148cf6ae1d89` | no |
| [gather-raw/stackstate-raw/cistate-D121848895.json](gather-raw/stackstate-raw/cistate-D121848895.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/cistate-D121848895.json` | 482 | `85ca6331269d3487` | no |
| [gather-raw/stackstate-raw/cistate-D121848896.json](gather-raw/stackstate-raw/cistate-D121848896.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/cistate-D121848896.json` | 483 | `70aa9974601a14c4` | no |
| [gather-raw/stackstate-raw/cistate-D121848898.json](gather-raw/stackstate-raw/cistate-D121848898.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/cistate-D121848898.json` | 482 | `9156003c438d2271` | no |
| [gather-raw/stackstate-raw/cistate-D121877477.json](gather-raw/stackstate-raw/cistate-D121877477.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/cistate-D121877477.json` | 482 | `64bbc8dbc34e24d8` | no |
| [gather-raw/stackstate-raw/cistate-D121877478.json](gather-raw/stackstate-raw/cistate-D121877478.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/cistate-D121877478.json` | 483 | `35cc662a91041998` | no |
| [gather-raw/stackstate-raw/cistate-D121877479.json](gather-raw/stackstate-raw/cistate-D121877479.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/cistate-D121877479.json` | 481 | `ac90962191efcdd4` | no |
| [gather-raw/stackstate-raw/cistate-D121877480.json](gather-raw/stackstate-raw/cistate-D121877480.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/cistate-D121877480.json` | 481 | `08d7b8bcff0ccebc` | no |
| [gather-raw/stackstate-raw/cistate-D121877481.json](gather-raw/stackstate-raw/cistate-D121877481.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/cistate-D121877481.json` | 482 | `ec24e87534b84c3a` | no |
| [gather-raw/stackstate-raw/cistate-D121978134.json](gather-raw/stackstate-raw/cistate-D121978134.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/cistate-D121978134.json` | 482 | `b7e4f8821f042952` | no |
| [gather-raw/stackstate-raw/cistate-D121978137.json](gather-raw/stackstate-raw/cistate-D121978137.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/cistate-D121978137.json` | 483 | `0ba6a50a8cf73bc1` | no |
| [gather-raw/stackstate-raw/diffs.txt](gather-raw/stackstate-raw/diffs.txt) | `/tmp/sub-tfa2-030/gather/stackstate-raw/diffs.txt` | 242 | `3ff5643799168d5f` | no |
| [gather-raw/stackstate-raw/files-D121244213.json](gather-raw/stackstate-raw/files-D121244213.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/files-D121244213.json` | 2,435 | `9d3d0b122bde0cea` | no |
| [gather-raw/stackstate-raw/files-D121244476.json](gather-raw/stackstate-raw/files-D121244476.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/files-D121244476.json` | 1,840 | `4ce0c70eab7880f2` | no |
| [gather-raw/stackstate-raw/files-D121649569.json](gather-raw/stackstate-raw/files-D121649569.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/files-D121649569.json` | 209 | `cbf7b8ee77b183a2` | no |
| [gather-raw/stackstate-raw/files-D121848525.json](gather-raw/stackstate-raw/files-D121848525.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/files-D121848525.json` | 396 | `14cf31cebaba7141` | no |
| [gather-raw/stackstate-raw/files-D121848885.json](gather-raw/stackstate-raw/files-D121848885.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/files-D121848885.json` | 816 | `be3915f6ac0d00f5` | no |
| [gather-raw/stackstate-raw/files-D121848886.json](gather-raw/stackstate-raw/files-D121848886.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/files-D121848886.json` | 1,270 | `0aeab485277fe278` | no |
| [gather-raw/stackstate-raw/files-D121848888.json](gather-raw/stackstate-raw/files-D121848888.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/files-D121848888.json` | 547 | `cd54b08bbe9fbea5` | no |
| [gather-raw/stackstate-raw/files-D121848889.json](gather-raw/stackstate-raw/files-D121848889.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/files-D121848889.json` | 650 | `8fae0f49d7399bb3` | no |
| [gather-raw/stackstate-raw/files-D121848890.json](gather-raw/stackstate-raw/files-D121848890.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/files-D121848890.json` | 309 | `6ea184ed9fbb27ed` | no |
| [gather-raw/stackstate-raw/files-D121848892.json](gather-raw/stackstate-raw/files-D121848892.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/files-D121848892.json` | 485 | `f24287aa89fad386` | no |
| [gather-raw/stackstate-raw/files-D121848893.json](gather-raw/stackstate-raw/files-D121848893.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/files-D121848893.json` | 492 | `361bd4b85209a022` | no |
| [gather-raw/stackstate-raw/files-D121848894.json](gather-raw/stackstate-raw/files-D121848894.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/files-D121848894.json` | 398 | `794f4f0b8d9421c4` | no |
| [gather-raw/stackstate-raw/files-D121848895.json](gather-raw/stackstate-raw/files-D121848895.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/files-D121848895.json` | 300 | `e53cad47c71f0ae1` | no |
| [gather-raw/stackstate-raw/files-D121848896.json](gather-raw/stackstate-raw/files-D121848896.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/files-D121848896.json` | 492 | `1b20c3e6cba57c50` | no |
| [gather-raw/stackstate-raw/files-D121848898.json](gather-raw/stackstate-raw/files-D121848898.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/files-D121848898.json` | 298 | `402bd41e23e78b5f` | no |
| [gather-raw/stackstate-raw/files-D121877477.json](gather-raw/stackstate-raw/files-D121877477.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/files-D121877477.json` | 592 | `12d1c709103b78c9` | no |
| [gather-raw/stackstate-raw/files-D121877478.json](gather-raw/stackstate-raw/files-D121877478.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/files-D121877478.json` | 300 | `04d2e515cf7281de` | no |
| [gather-raw/stackstate-raw/files-D121877479.json](gather-raw/stackstate-raw/files-D121877479.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/files-D121877479.json` | 485 | `99355742275f6b48` | no |
| [gather-raw/stackstate-raw/files-D121877480.json](gather-raw/stackstate-raw/files-D121877480.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/files-D121877480.json` | 392 | `7ccba766ffa9f433` | no |
| [gather-raw/stackstate-raw/files-D121877481.json](gather-raw/stackstate-raw/files-D121877481.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/files-D121877481.json` | 418 | `39774851a8dc2076` | no |
| [gather-raw/stackstate-raw/files-D121978134.json](gather-raw/stackstate-raw/files-D121978134.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/files-D121978134.json` | 416 | `f487900c2af3f175` | no |
| [gather-raw/stackstate-raw/files-D121978137.json](gather-raw/stackstate-raw/files-D121978137.json) | `/tmp/sub-tfa2-030/gather/stackstate-raw/files-D121978137.json` | 393 | `9c21d519e41d915a` | no |
| [gather-raw/stackstate-raw/sl-files-tfa-master.txt](gather-raw/stackstate-raw/sl-files-tfa-master.txt) | `/tmp/sub-tfa2-030/gather/stackstate-raw/sl-files-tfa-master.txt` | 55 | `d80b491e1ea5b021` | no |
| [gather-raw/stackstate-raw/sl-remote-master.txt](gather-raw/stackstate-raw/sl-remote-master.txt) | `/tmp/sub-tfa2-030/gather/stackstate-raw/sl-remote-master.txt` | 71 | `e6b1b41328cf1d9e` | no |
| [gather-raw/stackstate-raw/sl-stack-chain.txt](gather-raw/stackstate-raw/sl-stack-chain.txt) | `/tmp/sub-tfa2-030/gather/stackstate-raw/sl-stack-chain.txt` | 1,012 | `ef3d46fa606f2085` | no |
| [gather-raw/validate/rc.txt](gather-raw/validate/rc.txt) | `/tmp/sub-tfa2-030/gather/validate/rc.txt` | 990 | `e5341a223d534fdd` | no |
| [gather-raw/validate/summary.txt](gather-raw/validate/summary.txt) | `/tmp/sub-tfa2-030/gather/validate/summary.txt` | 967 | `788a40a892adc54d` | no |
| [gather-raw/validate/tfa-flaky-tests_20260917.out](gather-raw/validate/tfa-flaky-tests_20260917.out) | `/tmp/sub-tfa2-030/gather/validate/tfa-flaky-tests_20260917.out` | 124 | `7cc82d2791b67060` | no |
| [gather-raw/validate/tfa-kernel71-asyncssl-factor2-continuation_20260921.out](gather-raw/validate/tfa-kernel71-asyncssl-factor2-continuation_20260921.out) | `/tmp/sub-tfa2-030/gather/validate/tfa-kernel71-asyncssl-factor2-continuation_20260921.out` | 124 | `223f733e18cbc946` | no |
| [gather-raw/validate/tfa-kernel71-asyncssl-factor5-continuation_20260921.out](gather-raw/validate/tfa-kernel71-asyncssl-factor5-continuation_20260921.out) | `/tmp/sub-tfa2-030/gather/validate/tfa-kernel71-asyncssl-factor5-continuation_20260921.out` | 124 | `e601954154b5fa90` | no |
| [gather-raw/validate/tfa-kernel71-asyncssl-factor8-continuation_20260921.out](gather-raw/validate/tfa-kernel71-asyncssl-factor8-continuation_20260921.out) | `/tmp/sub-tfa2-030/gather/validate/tfa-kernel71-asyncssl-factor8-continuation_20260921.out` | 124 | `5e7f295f98879ea0` | no |
| [gather-raw/validate/tfa-kernel71-asyncssl-slice10-continuation_20260921.out](gather-raw/validate/tfa-kernel71-asyncssl-slice10-continuation_20260921.out) | `/tmp/sub-tfa2-030/gather/validate/tfa-kernel71-asyncssl-slice10-continuation_20260921.out` | 124 | `3e30942f1959ca5f` | no |
| [gather-raw/validate/tfa-kernel71-asyncssl-slice20-continuation_20260921.out](gather-raw/validate/tfa-kernel71-asyncssl-slice20-continuation_20260921.out) | `/tmp/sub-tfa2-030/gather/validate/tfa-kernel71-asyncssl-slice20-continuation_20260921.out` | 124 | `f518f0e8878834a6` | no |
| [gather-raw/validate/tfa-kernel71-asyncssl-slice5-continuation_20260921.out](gather-raw/validate/tfa-kernel71-asyncssl-slice5-continuation_20260921.out) | `/tmp/sub-tfa2-030/gather/validate/tfa-kernel71-asyncssl-slice5-continuation_20260921.out` | 123 | `60af69f5b9cfb090` | no |
| [gather-raw/validate/tfa-kernel71-asyncssl-slice50-continuation_20260921.out](gather-raw/validate/tfa-kernel71-asyncssl-slice50-continuation_20260921.out) | `/tmp/sub-tfa2-030/gather/validate/tfa-kernel71-asyncssl-slice50-continuation_20260921.out` | 124 | `e961314927b90ee8` | no |
| [gather-raw/validate/tfa-kernel71-campaign_20260921.out](gather-raw/validate/tfa-kernel71-campaign_20260921.out) | `/tmp/sub-tfa2-030/gather/validate/tfa-kernel71-campaign_20260921.out` | 128 | `c3952c59eec877c4` | no |
| [gather-raw/validate/tfa-kernel71-httpcoro-midpoint-continuation_20260921.out](gather-raw/validate/tfa-kernel71-httpcoro-midpoint-continuation_20260921.out) | `/tmp/sub-tfa2-030/gather/validate/tfa-kernel71-httpcoro-midpoint-continuation_20260921.out` | 121 | `9f2003aa9f954544` | no |
| [gather-raw/validate/tfa-kernel71-sched-continuation_20260921.out](gather-raw/validate/tfa-kernel71-sched-continuation_20260921.out) | `/tmp/sub-tfa2-030/gather/validate/tfa-kernel71-sched-continuation_20260921.out` | 124 | `02560da04e99bd1e` | no |
| [gather-raw/validate/tfa-kernel71-sched-input-campaign_20260921.out](gather-raw/validate/tfa-kernel71-sched-input-campaign_20260921.out) | `/tmp/sub-tfa2-030/gather/validate/tfa-kernel71-sched-input-campaign_20260921.out` | 124 | `212809fc7c29c246` | no |
| [gather-raw/validate/tfa-kernel71-thrift-slowdown-continuation_20260921.out](gather-raw/validate/tfa-kernel71-thrift-slowdown-continuation_20260921.out) | `/tmp/sub-tfa2-030/gather/validate/tfa-kernel71-thrift-slowdown-continuation_20260921.out` | 121 | `cbdbf84f6122d607` | no |
| [gather-raw/validate/tfa-kernel71-time-input-campaign_20260921.out](gather-raw/validate/tfa-kernel71-time-input-campaign_20260921.out) | `/tmp/sub-tfa2-030/gather/validate/tfa-kernel71-time-input-campaign_20260921.out` | 126 | `971ec515a91e63df` | no |
| [gather-raw/validate/unittest.out](gather-raw/validate/unittest.out) | `/tmp/sub-tfa2-030/gather/validate/unittest.out` | 160 | `3385c46a7080d5bf` | no |
| [gather-raw/validate/unittest.rc](gather-raw/validate/unittest.rc) | `/tmp/sub-tfa2-030/gather/validate/unittest.rc` | 23 | `ffc8e61e11ba11a9` | no |
| [gather-stackstate.md](gather-stackstate.md) | `/tmp/sub-tfa2-030/gather/stackstate.md` | 29,531 | `04426acf77a574a4` | no |
| [gather-wayforward.md](gather-wayforward.md) | `/tmp/sub-tfa2-030/gather/wayforward.md` | 41,824 | `be490acfefc700f6` | no |
| [gh/gh-campaign-1.json](gh/gh-campaign-1.json) | `/tmp/sub-tfa2-030/gh-campaign-1.json` | 295 | `af7336a1d0f0c28a` | no |
| [gh/gh-campaign-10.json](gh/gh-campaign-10.json) | `/tmp/sub-tfa2-030/gh-campaign-10.json` | 246 | `ba1c6d5bfe564a88` | no |
| [gh/gh-campaign-2.json](gh/gh-campaign-2.json) | `/tmp/sub-tfa2-030/gh-campaign-2.json` | 75 | `88c0f9b5efd367dc` | no |
| [gh/gh-campaign-3.json](gh/gh-campaign-3.json) | `/tmp/sub-tfa2-030/gh-campaign-3.json` | 355 | `3d50628aede81ed6` | no |
| [gh/gh-campaign-4.json](gh/gh-campaign-4.json) | `/tmp/sub-tfa2-030/gh-campaign-4.json` | 75 | `88c0f9b5efd367dc` | no |
| [gh/gh-campaign-5.json](gh/gh-campaign-5.json) | `/tmp/sub-tfa2-030/gh-campaign-5.json` | 523 | `3ed8ed92bc16f4ed` | no |
| [gh/gh-campaign-6.json](gh/gh-campaign-6.json) | `/tmp/sub-tfa2-030/gh-campaign-6.json` | 75 | `88c0f9b5efd367dc` | no |
| [gh/gh-campaign-7.json](gh/gh-campaign-7.json) | `/tmp/sub-tfa2-030/gh-campaign-7.json` | 265 | `ba7e99fa910ed39b` | no |
| [gh/gh-campaign-8.json](gh/gh-campaign-8.json) | `/tmp/sub-tfa2-030/gh-campaign-8.json` | 774 | `9f7f832e70fb37ec` | no |
| [gh/gh-campaign-9.json](gh/gh-campaign-9.json) | `/tmp/sub-tfa2-030/gh-campaign-9.json` | 75 | `88c0f9b5efd367dc` | no |
| [gh/gh-continuations-1.json](gh/gh-continuations-1.json) | `/tmp/sub-tfa2-030/gh-continuations-1.json` | 3,240 | `98270387d1808dae` | no |
| [gh/gh-continuations-2.json](gh/gh-continuations-2.json) | `/tmp/sub-tfa2-030/gh-continuations-2.json` | 355 | `3d50628aede81ed6` | no |
| [gh/gh-continuations-3.json](gh/gh-continuations-3.json) | `/tmp/sub-tfa2-030/gh-continuations-3.json` | 75 | `88c0f9b5efd367dc` | no |
| [gh/gh-continuations-4.json](gh/gh-continuations-4.json) | `/tmp/sub-tfa2-030/gh-continuations-4.json` | 1,321 | `6ae919a4bb583b17` | no |
| [gh/gh-continuations-5.json](gh/gh-continuations-5.json) | `/tmp/sub-tfa2-030/gh-continuations-5.json` | 258 | `b71df3c3e6387037` | no |
| [gh/gh-fixability-1.json](gh/gh-fixability-1.json) | `/tmp/sub-tfa2-030/gh-fixability-1.json` | 295 | `af7336a1d0f0c28a` | no |
| [gh/gh-fixability-10.json](gh/gh-fixability-10.json) | `/tmp/sub-tfa2-030/gh-fixability-10.json` | 1,339 | `285f694d23003241` | no |
| [gh/gh-fixability-11.json](gh/gh-fixability-11.json) | `/tmp/sub-tfa2-030/gh-fixability-11.json` | 75 | `88c0f9b5efd367dc` | no |
| [gh/gh-fixability-12.json](gh/gh-fixability-12.json) | `/tmp/sub-tfa2-030/gh-fixability-12.json` | 246 | `ba1c6d5bfe564a88` | no |
| [gh/gh-fixability-13.json](gh/gh-fixability-13.json) | `/tmp/sub-tfa2-030/gh-fixability-13.json` | 678 | `2ab01b1ea4a4752f` | no |
| [gh/gh-fixability-14.json](gh/gh-fixability-14.json) | `/tmp/sub-tfa2-030/gh-fixability-14.json` | 681 | `b36879a98da67d7f` | no |
| [gh/gh-fixability-15.json](gh/gh-fixability-15.json) | `/tmp/sub-tfa2-030/gh-fixability-15.json` | 425 | `f9a7c270df20e0e1` | no |
| [gh/gh-fixability-16.json](gh/gh-fixability-16.json) | `/tmp/sub-tfa2-030/gh-fixability-16.json` | 75 | `88c0f9b5efd367dc` | no |
| [gh/gh-fixability-17.json](gh/gh-fixability-17.json) | `/tmp/sub-tfa2-030/gh-fixability-17.json` | 272 | `c603f5d6da3a915a` | no |
| [gh/gh-fixability-18.json](gh/gh-fixability-18.json) | `/tmp/sub-tfa2-030/gh-fixability-18.json` | 265 | `ba7e99fa910ed39b` | no |
| [gh/gh-fixability-19.json](gh/gh-fixability-19.json) | `/tmp/sub-tfa2-030/gh-fixability-19.json` | 1,385 | `b547fb1d7916eb17` | no |
| [gh/gh-fixability-2.json](gh/gh-fixability-2.json) | `/tmp/sub-tfa2-030/gh-fixability-2.json` | 75 | `88c0f9b5efd367dc` | no |
| [gh/gh-fixability-20.json](gh/gh-fixability-20.json) | `/tmp/sub-tfa2-030/gh-fixability-20.json` | 4,764 | `dde04f4592f8fa3e` | no |
| [gh/gh-fixability-21.json](gh/gh-fixability-21.json) | `/tmp/sub-tfa2-030/gh-fixability-21.json` | 22,841 | `95adc5200d3c30ee` | no |
| [gh/gh-fixability-22.json](gh/gh-fixability-22.json) | `/tmp/sub-tfa2-030/gh-fixability-22.json` | 75 | `88c0f9b5efd367dc` | no |
| [gh/gh-fixability-23.json](gh/gh-fixability-23.json) | `/tmp/sub-tfa2-030/gh-fixability-23.json` | 619 | `80ec68d14b751213` | no |
| [gh/gh-fixability-24.json](gh/gh-fixability-24.json) | `/tmp/sub-tfa2-030/gh-fixability-24.json` | 75 | `88c0f9b5efd367dc` | no |
| [gh/gh-fixability-25.json](gh/gh-fixability-25.json) | `/tmp/sub-tfa2-030/gh-fixability-25.json` | 75 | `88c0f9b5efd367dc` | no |
| [gh/gh-fixability-26.json](gh/gh-fixability-26.json) | `/tmp/sub-tfa2-030/gh-fixability-26.json` | 75 | `88c0f9b5efd367dc` | no |
| [gh/gh-fixability-3.json](gh/gh-fixability-3.json) | `/tmp/sub-tfa2-030/gh-fixability-3.json` | 75 | `88c0f9b5efd367dc` | no |
| [gh/gh-fixability-4.json](gh/gh-fixability-4.json) | `/tmp/sub-tfa2-030/gh-fixability-4.json` | 355 | `3d50628aede81ed6` | no |
| [gh/gh-fixability-5.json](gh/gh-fixability-5.json) | `/tmp/sub-tfa2-030/gh-fixability-5.json` | 5,362 | `5fad8f1aaffe96b2` | no |
| [gh/gh-fixability-6.json](gh/gh-fixability-6.json) | `/tmp/sub-tfa2-030/gh-fixability-6.json` | 3,240 | `98270387d1808dae` | no |
| [gh/gh-fixability-7.json](gh/gh-fixability-7.json) | `/tmp/sub-tfa2-030/gh-fixability-7.json` | 2,345 | `3b60066cbf00415e` | no |
| [gh/gh-fixability-8.json](gh/gh-fixability-8.json) | `/tmp/sub-tfa2-030/gh-fixability-8.json` | 1,702 | `36d108dd4dbf53c4` | no |
| [gh/gh-fixability-9.json](gh/gh-fixability-9.json) | `/tmp/sub-tfa2-030/gh-fixability-9.json` | 75 | `88c0f9b5efd367dc` | no |
| [gh/gh-phase2-1.json](gh/gh-phase2-1.json) | `/tmp/sub-tfa2-030/gh-phase2-1.json` | 355 | `3d50628aede81ed6` | no |
| [gh/gh-phase2-2.json](gh/gh-phase2-2.json) | `/tmp/sub-tfa2-030/gh-phase2-2.json` | 202 | `4d2ad177f7416a25` | no |
| [gh/gh-phase2-3.json](gh/gh-phase2-3.json) | `/tmp/sub-tfa2-030/gh-phase2-3.json` | 113 | `aa3d99c14fe1af12` | no |
| [gh/gh-stackstate-1.json](gh/gh-stackstate-1.json) | `/tmp/sub-tfa2-030/gh-stackstate-1.json` | 355 | `3d50628aede81ed6` | no |
| [gh/gh-stackstate-2.json](gh/gh-stackstate-2.json) | `/tmp/sub-tfa2-030/gh-stackstate-2.json` | 75 | `88c0f9b5efd367dc` | no |
| [gh/gh-wayforward-1.json](gh/gh-wayforward-1.json) | `/tmp/sub-tfa2-030/gh-wayforward-1.json` | 343 | `526d0515a33c5ba8` | no |
| [gh/gh-wayforward-2.json](gh/gh-wayforward-2.json) | `/tmp/sub-tfa2-030/gh-wayforward-2.json` | 177 | `d3286becc45d9c8d` | no |
| [gh/gh-wayforward-3.json](gh/gh-wayforward-3.json) | `/tmp/sub-tfa2-030/gh-wayforward-3.json` | 136 | `043da597cda2595a` | no |
| [gh/gh-wayforward-4.json](gh/gh-wayforward-4.json) | `/tmp/sub-tfa2-030/gh-wayforward-4.json` | 355 | `3d50628aede81ed6` | no |
| [gh/gh-wayforward-5.json](gh/gh-wayforward-5.json) | `/tmp/sub-tfa2-030/gh-wayforward-5.json` | 75 | `88c0f9b5efd367dc` | no |
| [gh/gh-wayforward-6.json](gh/gh-wayforward-6.json) | `/tmp/sub-tfa2-030/gh-wayforward-6.json` | 75 | `88c0f9b5efd367dc` | no |
| [gh/gh-wayforward-7.json](gh/gh-wayforward-7.json) | `/tmp/sub-tfa2-030/gh-wayforward-7.json` | 177 | `322c134ac9227818` | no |
| [gh/gh-wayforward-8.json](gh/gh-wayforward-8.json) | `/tmp/sub-tfa2-030/gh-wayforward-8.json` | 302 | `9d97281407d05561` | no |
| [phase2-three-case-RESULTS.md](phase2-three-case-RESULTS.md) | `/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/RESULTS.md` | 24,846 | `30342368daeb13ed` | no |
| [tfa-lane/codex-drift/AUDIT.md](tfa-lane/codex-drift/AUDIT.md) | `/home/newton/work/tfa/codex-drift/AUDIT.md` | 1,864 | `efd09f1bf2d834cf` | no |
| [tfa-lane/codex-rollover/full-test.log.txt](tfa-lane/codex-rollover/full-test.log.txt) | `/home/newton/work/tfa/codex-rollover/full-test.log` | 4,990 | `e20db8f0331b7053` | no |
| [tfa-lane/codex-rollover/summary.txt](tfa-lane/codex-rollover/summary.txt) | `/home/newton/work/tfa/codex-rollover/summary.txt` | 1,647 | `f5ed97dd61c5114f` | no |
| [tfa-lane/codex-rollover/testplan.txt](tfa-lane/codex-rollover/testplan.txt) | `/home/newton/work/tfa/codex-rollover/testplan.txt` | 1,107 | `d239e24623429b7e` | no |
| [tfa-lane/external-hermit/witnesses-1667ce0d7600-fixture/SUMMARY.txt](tfa-lane/external-hermit/witnesses-1667ce0d7600-fixture/SUMMARY.txt) | `/home/newton/work/tfa/external-hermit/witnesses-1667ce0d7600-fixture/SUMMARY.txt` | 476 | `2f43078353b3d37e` | no |
| [tfa-lane/external-hermit/witnesses-1667ce0d7600/SUMMARY.txt](tfa-lane/external-hermit/witnesses-1667ce0d7600/SUMMARY.txt) | `/home/newton/work/tfa/external-hermit/witnesses-1667ce0d7600/SUMMARY.txt` | 475 | `055281d626f2754d` | no |
| [tfa-lane/external-hermit/witnesses-2b5edff7bfde-fixture/SUMMARY.txt](tfa-lane/external-hermit/witnesses-2b5edff7bfde-fixture/SUMMARY.txt) | `/home/newton/work/tfa/external-hermit/witnesses-2b5edff7bfde-fixture/SUMMARY.txt` | 582 | `538b67ebc6a2b27f` | no |
| [tfa-lane/external-hermit/witnesses-418be8592ff2-fixture/SUMMARY.txt](tfa-lane/external-hermit/witnesses-418be8592ff2-fixture/SUMMARY.txt) | `/home/newton/work/tfa/external-hermit/witnesses-418be8592ff2-fixture/SUMMARY.txt` | 475 | `22fabe83cd202cb2` | no |
| [tfa-lane/external-hermit/witnesses-4fb8c4120739-fixture/SUMMARY.txt](tfa-lane/external-hermit/witnesses-4fb8c4120739-fixture/SUMMARY.txt) | `/home/newton/work/tfa/external-hermit/witnesses-4fb8c4120739-fixture/SUMMARY.txt` | 883 | `99137541f5c91de4` | no |
| [tfa-lane/external-hermit/witnesses-55f6e37a4a63-fixture/SUMMARY.txt](tfa-lane/external-hermit/witnesses-55f6e37a4a63-fixture/SUMMARY.txt) | `/home/newton/work/tfa/external-hermit/witnesses-55f6e37a4a63-fixture/SUMMARY.txt` | 475 | `71a54b5d2d31294f` | no |
| [tfa-lane/external-hermit/witnesses-dc3528a0d1ad-fixture/SUMMARY.txt](tfa-lane/external-hermit/witnesses-dc3528a0d1ad-fixture/SUMMARY.txt) | `/home/newton/work/tfa/external-hermit/witnesses-dc3528a0d1ad-fixture/SUMMARY.txt` | 475 | `5224aade1c61e5a0` | no |
| [tfa-lane/external-oss-mode/D121978137-test-plan.txt](tfa-lane/external-oss-mode/D121978137-test-plan.txt) | `/home/newton/work/tfa/external-oss-mode/D121978137-test-plan.txt` | 2,500 | `a2d3839a899e425d` | no |
| [tfa-lane/external-oss-mode/DISK-HOLD-4fb8c4120739.txt](tfa-lane/external-oss-mode/DISK-HOLD-4fb8c4120739.txt) | `/home/newton/work/tfa/external-oss-mode/DISK-HOLD-4fb8c4120739.txt` | 7,415 | `43045035e4f462a2` | no |
| [tfa-lane/external-oss-mode/mutants-head6/SUMMARY.txt](tfa-lane/external-oss-mode/mutants-head6/SUMMARY.txt) | `/home/newton/work/tfa/external-oss-mode/mutants-head6/SUMMARY.txt` | 2,903 | `077b1d2cfc835025` | no |
| [tfa-lane/hermit-provenance/AUDIT.md](tfa-lane/hermit-provenance/AUDIT.md) | `/home/newton/work/tfa/hermit-provenance/AUDIT.md` | 4,799 | `b4f7194ca6dc36b0` | no |
| [tfa-lane/import-watcher/watch.jsonl](tfa-lane/import-watcher/watch.jsonl) | `/home/newton/work/tfa/import-watcher/watch.jsonl` | 4,305 | `de0129893d116ccb` | no |
| [tfa-lane/sudo-fixture/AUDIT.md](tfa-lane/sudo-fixture/AUDIT.md) | `/home/newton/work/tfa/sudo-fixture/AUDIT.md` | 4,518 | `7b2d17bb56b8b5da` | no |
| [tfa-lane/three-case-0a413f6c/SUMMARY.txt](tfa-lane/three-case-0a413f6c/SUMMARY.txt) | `/home/newton/work/tfa/three-case-0a413f6c/SUMMARY.txt` | 724 | `c35233206f9c5ec5` | no |
| [tfa-lane/three-case-b6b8f7a16a8a/SUMMARY.txt](tfa-lane/three-case-b6b8f7a16a8a/SUMMARY.txt) | `/home/newton/work/tfa/three-case-b6b8f7a16a8a/SUMMARY.txt` | 1,014 | `244cccbb2cf590f2` | no |
| [tfa-lane/three-case/SUMMARY.txt](tfa-lane/three-case/SUMMARY.txt) | `/home/newton/work/tfa/three-case/SUMMARY.txt` | 808 | `728155e7ce9af402` | no |
| [ti-timerfd.json](ti-timerfd.json) | `/tmp/sub-tfa2-030/ti-timerfd.json` | 2,598 | `95827cd244e788ba` | no |
| [ti/281475235281183.json](ti/281475235281183.json) | `/tmp/sub-tfa2-030/ti/281475235281183.json` | 48 | `a8723df56989611b` | no |
| [ti/281475235299286.json](ti/281475235299286.json) | `/tmp/sub-tfa2-030/ti/281475235299286.json` | 1,059 | `9fdfed32465df6bb` | no |
| [ti/281475236296488.json](ti/281475236296488.json) | `/tmp/sub-tfa2-030/ti/281475236296488.json` | 3,458 | `e4efbd94e77309f0` | no |
| [ti/281475238796418.json](ti/281475238796418.json) | `/tmp/sub-tfa2-030/ti/281475238796418.json` | 3,797 | `16b836d3d64069bc` | no |
| [ti/281475240048871.json](ti/281475240048871.json) | `/tmp/sub-tfa2-030/ti/281475240048871.json` | 3,180 | `2c84427f2e92a3dd` | no |
| [ti/281475240064369.json](ti/281475240064369.json) | `/tmp/sub-tfa2-030/ti/281475240064369.json` | 2,974 | `94ad88fea7f07297` | no |
| [ti/281475243043222.json](ti/281475243043222.json) | `/tmp/sub-tfa2-030/ti/281475243043222.json` | 3,080 | `1cc620efa5309083` | no |
| [ti/281475243051108.json](ti/281475243051108.json) | `/tmp/sub-tfa2-030/ti/281475243051108.json` | 3,160 | `757e3f703eacac33` | no |
| [ti/281475243249473.json](ti/281475243249473.json) | `/tmp/sub-tfa2-030/ti/281475243249473.json` | 2,930 | `7a24999fb41cbfcc` | no |
| [ti/281475243259972.json](ti/281475243259972.json) | `/tmp/sub-tfa2-030/ti/281475243259972.json` | 2,855 | `1a628da92ada34cd` | no |
| [ti/281475244092464.json](ti/281475244092464.json) | `/tmp/sub-tfa2-030/ti/281475244092464.json` | 2,663 | `86b8eb4b8eb8be92` | no |
| [ti/281475250915202.json](ti/281475250915202.json) | `/tmp/sub-tfa2-030/ti/281475250915202.json` | 3,410 | `b14d0fd92ef1383d` | no |
| [ti/281475271465112.json](ti/281475271465112.json) | `/tmp/sub-tfa2-030/ti/281475271465112.json` | 48 | `a8723df56989611b` | no |
| [ti/562950211052906.json](ti/562950211052906.json) | `/tmp/sub-tfa2-030/ti/562950211052906.json` | 5,609 | `54ed7ff239f69941` | no |
| [ti/562950212033102.json](ti/562950212033102.json) | `/tmp/sub-tfa2-030/ti/562950212033102.json` | 3,291 | `12d9a727ca179bc0` | no |
| [ti/562950213120998.json](ti/562950213120998.json) | `/tmp/sub-tfa2-030/ti/562950213120998.json` | 3,502 | `bcb9207107f5274a` | no |
| [ti/562950215319283.json](ti/562950215319283.json) | `/tmp/sub-tfa2-030/ti/562950215319283.json` | 3,175 | `abde17d47a5bf434` | no |
| [ti/562950215341224.json](ti/562950215341224.json) | `/tmp/sub-tfa2-030/ti/562950215341224.json` | 48 | `a8723df56989611b` | no |
| [ti/562950216898741.json](ti/562950216898741.json) | `/tmp/sub-tfa2-030/ti/562950216898741.json` | 4,067 | `15a106842fdcd0ee` | no |
| [ti/562950220120091.json](ti/562950220120091.json) | `/tmp/sub-tfa2-030/ti/562950220120091.json` | 3,327 | `d75bc80ca7d49454` | no |
| [ti/562950242844554.json](ti/562950242844554.json) | `/tmp/sub-tfa2-030/ti/562950242844554.json` | 3,993 | `1969ff8c071457fe` | no |
| [ti/562950274089363.json](ti/562950274089363.json) | `/tmp/sub-tfa2-030/ti/562950274089363.json` | 9,287 | `4d0c0944051d9029` | no |
| [ti/562950354325343.json](ti/562950354325343.json) | `/tmp/sub-tfa2-030/ti/562950354325343.json` | 2,531 | `dd70265c762794a0` | no |
| [ti/562950361061616.json](ti/562950361061616.json) | `/tmp/sub-tfa2-030/ti/562950361061616.json` | 3,599 | `3541256972001c9a` | no |
| [ti/844425193659677.json](ti/844425193659677.json) | `/tmp/sub-tfa2-030/ti/844425193659677.json` | 2,798 | `52dc738cff36422c` | no |
| [ti/844425197050049.json](ti/844425197050049.json) | `/tmp/sub-tfa2-030/ti/844425197050049.json` | 4,323 | `ae4cb324fd7dc61a` | no |
| [ti/844425202171362.json](ti/844425202171362.json) | `/tmp/sub-tfa2-030/ti/844425202171362.json` | 9,357 | `23f31622a234e02f` | no |
| [ti/844425205465381.json](ti/844425205465381.json) | `/tmp/sub-tfa2-030/ti/844425205465381.json` | 9,158 | `fcbbfae64c94e5d0` | no |
| [ti/844425263427177.json](ti/844425263427177.json) | `/tmp/sub-tfa2-030/ti/844425263427177.json` | 3,231 | `24d374bbf63749a9` | no |
| [ti/rc.txt](ti/rc.txt) | `/tmp/sub-tfa2-030/ti/rc.txt` | 609 | `84d8c2c52d7d9f5f` | no |
| [ti/run-get-281475236296488.txt](ti/run-get-281475236296488.txt) | `/tmp/sub-tfa2-030/ti/run-get-281475236296488.txt` | 19,728 | `bb06c12c50158bfb` | no |
| [ti/run-get-281475243249473.txt](ti/run-get-281475243249473.txt) | `/tmp/sub-tfa2-030/ti/run-get-281475243249473.txt` | 19,787 | `23a69b43330cdeed` | no |
| [ti/run-get-281475244092464.txt](ti/run-get-281475244092464.txt) | `/tmp/sub-tfa2-030/ti/run-get-281475244092464.txt` | 19,737 | `c8898053d394d5c4` | no |
| [ti/run-get-562950212033102.txt](ti/run-get-562950212033102.txt) | `/tmp/sub-tfa2-030/ti/run-get-562950212033102.txt` | 19,764 | `8b720dc184c2fdf9` | no |
| [ti/run-get-562950216898741.txt](ti/run-get-562950216898741.txt) | `/tmp/sub-tfa2-030/ti/run-get-562950216898741.txt` | 16,233 | `f30405e143717b62` | no |
| [ti/run-get-562950220120091.txt](ti/run-get-562950220120091.txt) | `/tmp/sub-tfa2-030/ti/run-get-562950220120091.txt` | 19,755 | `93a0fe6261106bf0` | no |
| [ti/run-get-844425197050049.txt](ti/run-get-844425197050049.txt) | `/tmp/sub-tfa2-030/ti/run-get-844425197050049.txt` | 19,770 | `0f1d2802e3ebc137` | no |
| [ti/run.log.txt](ti/run.log.txt) | `/tmp/sub-tfa2-030/ti/run.log` | 5 | `d117fa006ba92085` | no |
| [ti/run.sh](ti/run.sh) | `/tmp/sub-tfa2-030/ti/run.sh` | 933 | `4ade2a7643b813eb` | no |
| [ti/summary.txt](ti/summary.txt) | `/tmp/sub-tfa2-030/ti/summary.txt` | 25,176 | `70f1a2a839b3c04c` | no |
| [ti/tls-meta-full.json](ti/tls-meta-full.json) | `/tmp/sub-tfa2-030/ti/tls-meta-full.json` | 523 | `8c16461b1430220f` | no |
| [ti/tls-meta.json](ti/tls-meta.json) | `/tmp/sub-tfa2-030/ti/tls-meta.json` | 598 | `fb14c43f299c790c` | no |
| [ti/tls-run-get.txt](ti/tls-run-get.txt) | `/tmp/sub-tfa2-030/ti/tls-run-get.txt` | 7,706 | `0e9515f0d52927d1` | no |
| [tmp-tfa-phase2/goal-20260923.CQi0pX/controller-followup-audit.md](tmp-tfa-phase2/goal-20260923.CQi0pX/controller-followup-audit.md) | `/tmp/tfa-phase2/goal-20260923.CQi0pX/controller-followup-audit.md` | 10,946 | `adc4529578760100` | no |
| [tmp-tfa-phase2/production-full-68cf2bac-recheck.out](tmp-tfa-phase2/production-full-68cf2bac-recheck.out) | `/tmp/tfa-phase2/production-full-68cf2bac-recheck.out` | 22,525 | `1ec896092fcc992f` | yes |
| [tmp-tfa-phase2/requal-a919-asyncssl-pass-seed1-single-asan/result.json](tmp-tfa-phase2/requal-a919-asyncssl-pass-seed1-single-asan/result.json) | `/tmp/tfa-phase2/requal-a919-asyncssl-pass-seed1-single-asan/result.json` | 1,346 | `cab7baf3c3bde284` | no |
| [tmp-tfa-phase2/requal-a919-asyncssl-pass-seed1-single-asan1/result.json](tmp-tfa-phase2/requal-a919-asyncssl-pass-seed1-single-asan1/result.json) | `/tmp/tfa-phase2/requal-a919-asyncssl-pass-seed1-single-asan1/result.json` | 1,348 | `b41471dd7820740e` | no |
| [tmp-tfa-phase2/requal-a919-asyncssl-pass-seed1-single/result.json](tmp-tfa-phase2/requal-a919-asyncssl-pass-seed1-single/result.json) | `/tmp/tfa-phase2/requal-a919-asyncssl-pass-seed1-single/result.json` | 1,298 | `8f0844ce6a61e846` | no |
| [tmp-tfa-phase2/requal-a919-asyncssl-pass-seed1-v2/result.json](tmp-tfa-phase2/requal-a919-asyncssl-pass-seed1-v2/result.json) | `/tmp/tfa-phase2/requal-a919-asyncssl-pass-seed1-v2/result.json` | 1,290 | `4b1ddfd0e44713bd` | no |
| [tmp-tfa-phase2/requal-a919-asyncssl-pass-seed1-v3/result.json](tmp-tfa-phase2/requal-a919-asyncssl-pass-seed1-v3/result.json) | `/tmp/tfa-phase2/requal-a919-asyncssl-pass-seed1-v3/result.json` | 1,332 | `12d00d944974eab0` | no |
| [tmp-tfa-phase2/requal-a919-asyncssl-pass-seed1/result.json](tmp-tfa-phase2/requal-a919-asyncssl-pass-seed1/result.json) | `/tmp/tfa-phase2/requal-a919-asyncssl-pass-seed1/result.json` | 1,410 | `ca211d06db68130c` | no |
| [tmp-tfa-phase2/requal-a919-httpcoro-pass-seed1-single/result.json](tmp-tfa-phase2/requal-a919-httpcoro-pass-seed1-single/result.json) | `/tmp/tfa-phase2/requal-a919-httpcoro-pass-seed1-single/result.json` | 1,323 | `4d3f743bd2935755` | no |
| [tmp-tfa-phase2/requal-a919-thrift-pass-seed0-single/result.json](tmp-tfa-phase2/requal-a919-thrift-pass-seed0-single/result.json) | `/tmp/tfa-phase2/requal-a919-thrift-pass-seed0-single/result.json` | 1,319 | `9125d939656b34ed` | no |
| [tmp-tfa-phase2/tfa-phase2-controller-followup-asyncssl-seed1-driver.log.txt](tmp-tfa-phase2/tfa-phase2-controller-followup-asyncssl-seed1-driver.log.txt) | `/tmp/tfa-phase2-controller-followup-asyncssl-seed1-driver.log` | 214 | `37b96ac8a2f17854` | no |
| [tmp-tfa-phase2/tfa-phase2-controller-followup-asyncssl-seed11-driver.log.txt](tmp-tfa-phase2/tfa-phase2-controller-followup-asyncssl-seed11-driver.log.txt) | `/tmp/tfa-phase2-controller-followup-asyncssl-seed11-driver.log` | 216 | `cc0256ad85e69fae` | no |
| [tmp-tfa-phase2/tfa-phase2-controller-followup-asyncssl-stress-driver.log.txt](tmp-tfa-phase2/tfa-phase2-controller-followup-asyncssl-stress-driver.log.txt) | `/tmp/tfa-phase2-controller-followup-asyncssl-stress-driver.log` | 116 | `d78ed15be84f6647` | no |
| [tmp-tfa-phase2/tfa-phase2-controller-followup-asyncssl-stress-verify.log.txt](tmp-tfa-phase2/tfa-phase2-controller-followup-asyncssl-stress-verify.log.txt) | `/tmp/tfa-phase2-controller-followup-asyncssl-stress-verify.log` | 688 | `d48a94aa919a74f3` | no |
| [tmp-tfa-phase2/tfa-phase2-controller-followup-real-asyncssl-verify.log.txt](tmp-tfa-phase2/tfa-phase2-controller-followup-real-asyncssl-verify.log.txt) | `/tmp/tfa-phase2-controller-followup-real-asyncssl-verify.log` | 295 | `9195e7ea8e04accf` | no |
| [tmp-tfa-phase2/tfa-phase2-controller-followup-real-asyncssl.log.txt](tmp-tfa-phase2/tfa-phase2-controller-followup-real-asyncssl.log.txt) | `/tmp/tfa-phase2-controller-followup-real-asyncssl.log` | 418 | `08abb7bd070349b2` | no |
| [tmp-tfa-phase2/tfa-phase2-controller-followup-real-httpcoro.log.txt](tmp-tfa-phase2/tfa-phase2-controller-followup-real-httpcoro.log.txt) | `/tmp/tfa-phase2-controller-followup-real-httpcoro.log` | 62 | `8b05b2751e40e9b3` | no |
| [tmp-tfa-phase2/tfa-phase2-production-final.out](tmp-tfa-phase2/tfa-phase2-production-final.out) | `/tmp/tfa-phase2-production-final.out` | 22,043 | `f9b738da82529424` | yes |
| [tmp-tfa-phase2/unit-68cf2bac-recheck.out](tmp-tfa-phase2/unit-68cf2bac-recheck.out) | `/tmp/tfa-phase2/unit-68cf2bac-recheck.out` | 508 | `6ecbd125b7b4baae` | no |
| [way/d121815443.json](way/d121815443.json) | `/tmp/sub-tfa2-030/way/d121815443.json` | 8,243 | `25276e845ca49906` | no |
| [way/hermit-test-meta.json](way/hermit-test-meta.json) | `/tmp/sub-tfa2-030/way/hermit-test-meta.json` | 89,937 | `2eb0e9dd3c7d580f` | no |
| [verify/acl-inspect.txt](verify/acl-inspect.txt) | `/tmp/sub-tfa2-030/verify/acl-inspect.txt` (`agentcloudctl inspect`) | 477 | `14b493d53cd6717a` | no |
| [verify/devbig030.json](verify/devbig030.json) | `/tmp/sub-tfa2-030/verify/devbig030.json` (`meta devserver.server describe`) | 500 | `f50102187ab94c9e` | no |
| [verify/devvm895.json](verify/devvm895.json) | `/tmp/sub-tfa2-030/verify/devvm895.json` (`meta devserver.server describe`) | 515 | `aa7fe60895a30681` | no |
| [verify/hist-asyncssl.json](verify/hist-asyncssl.json) | `/tmp/sub-tfa2-030/verify/hist-asyncssl.json` (`meta testinfra.result history --test=562950211052906 --since=ONE_WEEK --final-only --limit=200`) | 164,334 | `3949b2d36f846243` | no |
| [verify/tg-futuretimeout-v.txt](verify/tg-futuretimeout-v.txt) | `/tmp/sub-tfa2-030/verify/tg-futuretimeout-v.txt` (`tg -v tfa-diagnose-futuretimeout-strict-hermit`) | 11,288 | `866927aa8016106d` | no |
| [verify/ti-hist-562950215319283.json](verify/ti-hist-562950215319283.json) | `/tmp/sub-tfa2-030/verify/ti-hist-562950215319283.json` (`meta testinfra.result history --test=562950215319283`) | 92,721 | `6400a272aedcce27` | no |

## Cited but not copied

| Original | Why not copied |
|---|---|
| `/home/newton/work/dev-hermit/ignored/safehermit-loopback/mnt/safehermit/` (retained Hermit logs) | Large; untracked but inside the dev-hermit checkout. The cited lines are quoted in `gather-fixability.md` §1 and `../EXPERIMENTS.md` §4. |
| `/tmp/sub-tfa2-030/devvm895/tfa-src/fbcode/testinfra/tfa/` (stack-tip source, 1.4 MB) | fbsource source code; over the size budget. Its diff summaries are copied as `devvm895/STACK_SUMMARIES.md`. The same code is in the 22 Phabricator drafts. |
| `/tmp/sub-tfa2-030/gather/campaign_recount.out` (456,604 bytes) | Over 200 KB. `gather-raw/campaign_recount.py` regenerates it read-only from the committed records. |
| `/tmp/sub-tfa2-030/devvm895/tfa-diag/results-dev.json` (243,313 bytes) | Over 200 KB. It is the CI result for TestX run 1125900439774673, which can be re-queried. |
| `/tmp/sub-tfa2-030/gather/stackstate-raw/` (all other outputs, about 3.1 MB) | Over the size budget; only `files-D*`, `cistate-D*`, `sl-*` and `diffs.txt` were copied. The rest is summarized in `gather-stackstate.md`. |
| `/tmp/sub-tfa2-030/gather/files-D*.json` | Duplicates of `gather-raw/stackstate-raw/files-D*.json` (3 of the 22 also carry an appended CLI hint). |
| `worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/` (all but `RESULTS.md`) and `.../tfa-phase2-evidence/` | Candidate patches to fbsource tests, receipts and logs. Preserving them is next step 2 in `../WAY_FORWARD.md`, an owner decision. |
| `/home/newton/work/tfa/` (other files: mutant logs, witness logs, the fbsource tree) | Only the summaries and audits cited by the documents were copied. The codex-rollover mutant outputs contain unredacted signed links. |
| `/tmp/tfa-phase2/` (other files) | Only the files cited by the documents were copied. |
