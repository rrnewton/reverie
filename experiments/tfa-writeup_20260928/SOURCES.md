# TFA write-up: source keys and first-draft provenance

This file keeps the source-key table that the appendices cite (for example `[CAMP §5]`, `[RES §4.1]`,
`[LOGS]`). It was the Sources section of the first draft of README.md (dev-hermit commit
`24e90ece253d94776353feccd90faf87d87c46ef`). The revised README of 2026-09-29 links files directly, and
[metadata.json](metadata.json) holds the full SHAs and hashes.

## First-draft provenance

Written 2026-09-28 by sub-tfa2-030, a read-only Claude subagent, for the owner. No test or Hermit run was
repeated for the first draft. Repository records were read at dev-hermit
`1393fcda8e20c4b06f0aec5b32f071a6e9a8fd40` (`main`, 2026-09-28T08:59:22-07:00). The TFA records were
identical at the then-newer `origin/main` `864ab69db945`. The other sources are listed in
[EXPERIMENTS.md section 9](EXPERIMENTS.md#9-provenance-of-this-write-up).

## Source keys

Repository paths are relative to `/home/newton/work/dev-hermit`. Files under
`/tmp/sub-tfa2-030/` are this write-up's evidence notes (2026-09-28) and are not durable; the
"Copy" column gives the copy kept in this directory. The copies of the gatherer notes are
verbatim, so paths inside them still name the `/tmp` originals; translate them with
[evidence/MANIFEST.md](evidence/MANIFEST.md).

| Key | Location | Copy in this directory |
|---|---|---|
| RES | `experiments/tfa-kernel71-campaign_20260921/RESULTS.md` (the campaign's own report) | tracked |
| ACC | `ai_docs/tfa-acceptance-bracket-2026-09-17.md` (success rule; commit `9d7f603c4`) | tracked |
| SRC | `ai_docs/tfa-candidate-sourcing-2026-09-17.md` (registry method; commit `ee6562142`) | tracked |
| COH | `experiments/tfa-kernel71-campaign_20260921/cohort.json` (the 30 targets) | tracked |
| SCR | `experiments/tfa-kernel71-campaign_20260921/NATIVE-CANDIDATE-SCREEN-20260921.md` | tracked |
| K71 | `experiments/tfa-kernel71-*_20260921/services/*/*/experiment.json` (25 records, 13 roots) | tracked |
| K613 | `experiments/tfa-flaky-tests_20260917/services/*/*/experiment.json` (10 pilot records) | tracked |
| PROBE | `experiments/tfa-openat-stack-size-20260917/` (openat stack-size reproducer) | tracked |
| LOGS | `ignored/safehermit-loopback/mnt/safehermit/` (retained Hermit logs; untracked) | not copied (large); the cited lines are quoted in [evidence/gather-fixability.md](evidence/gather-fixability.md) §1 and [EXPERIMENTS.md](EXPERIMENTS.md) §4 |
| TI | `/tmp/sub-tfa2-030/ti/<test id>.json` and `/tmp/sub-tfa2-030/ti-timerfd.json` (`meta testinfra.result history --test=<id> --statuses=FAILED,FATAL,TIMEOUT --since=ONE_WEEK --limit=3 --output=json`, 2026-09-28) | [evidence/ti/](evidence/ti/), [evidence/ti-timerfd.json](evidence/ti-timerfd.json) |
| CAMP | `/tmp/sub-tfa2-030/gather/campaign.md`, with `campaign_recount.py`/`.out` and `campaign_verify_classify.py`/`.out` | [evidence/gather-campaign.md](evidence/gather-campaign.md); scripts in [evidence/gather-raw/](evidence/gather-raw/). The 456,604-byte `campaign_recount.out` was not copied; rerunning the read-only `campaign_recount.py` regenerates it |
| CONT | `/tmp/sub-tfa2-030/gather/continuations.md` | [evidence/gather-continuations.md](evidence/gather-continuations.md) |
| FIX | `/tmp/sub-tfa2-030/gather/fixability.md` | [evidence/gather-fixability.md](evidence/gather-fixability.md) |
| P2 | `/tmp/sub-tfa2-030/gather/phase2.md` | [evidence/gather-phase2.md](evidence/gather-phase2.md) |
| WF | `/tmp/sub-tfa2-030/gather/wayforward.md` | [evidence/gather-wayforward.md](evidence/gather-wayforward.md) |
| SS | `/tmp/sub-tfa2-030/gather/stackstate.md` | [evidence/gather-stackstate.md](evidence/gather-stackstate.md) |
| E3 | `worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/RESULTS.md` (phase-two report; untracked) | [evidence/phase2-three-case-RESULTS.md](evidence/phase2-three-case-RESULTS.md) |
| EXP, TGT | [EXPERIMENTS.md](EXPERIMENTS.md), [TARGETS.md](TARGETS.md) in this directory | – |

