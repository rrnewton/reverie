# Four unchanged futex enrollment controls

All four named existing methods passed on their first focused attempts on commit `9db60ab95587d4cb5e0438dfeca409471eb9baf5`, using its already emitted and separately retained source-v23 library ELF. No source, ref, test, assertion, timeout, selector or observer was changed, and no build was run. This does not clear or relabel the earlier full-library result of 426 passes and one failure among 427 methods.

| Method | Actual CPU seconds | Observed service wall seconds | Result |
|---|---:|---:|---|
| `vm::tests::fatal_worker_ro_delayed_waiter_qualifies` | 0.455962 | 7.134291836 | One passed; zero failed or ignored |
| `vm::tests::fatal_worker_rw_delayed_waiter_qualifies` | 0.424450 | 3.855429346 | One passed; zero failed or ignored |
| `vm::tests::fatal_worker_ro_failed_store_still_wakes` | 0.281353 | 3.231693839 | One passed; zero failed or ignored |
| `vm::tests::fatal_worker_rw_store_wake_and_slot_release` | 0.320440 | 4.031627177 | One passed; zero failed or ignored |

The four services totalled 1.482205 CPU seconds and 18.253042198 seconds summed over their individual observed wall times. These service durations include observer/service admission and are not the one-second futex interval. Libtest reported 0.48, 0.30, 0.14 and 0.10 seconds for the four methods respectively. No inference about the historical host scheduling cause follows from these timings.

Each invocation selected exactly its named method from the already recorded 453-method inventory, with 452 filtered out. Each had actual `/dev/kvm` API 12 admission in its observed service and `REVERIE_REQUIRE_KVM=1`; no optional no-KVM return or skip was accepted. All stdout was read, all stderr was empty, and raw output was bounded and untruncated. All original 30 CPU / 60 wall seconds, 16 GiB memory / zero swap and 1 MiB output limits remained. Actual payloads exited 0 with complete aggregate accounting. Existing postchecks and four additional fresh systemctl readbacks establish inactive services, MainPID 0 and empty ControlGroup.

## Historical failure and phase

The original native-full-v1/v2 caller ran the full, unfiltered 427-method library once under the same 30 CPU / 60 wall bounds. Full raw JSON outcome parsing confirms 426 passes, the single failure in `vm::tests::fatal_worker_ro_delayed_waiter_qualifies`, and passes for the other three methods. Its payload exited 101 after 8.260615 CPU / 21.283923545 observed wall seconds, with complete accounting and no observer refusal. The complete original stderr was read; it says reverse requeue returned 0 instead of 1, with waiter result `Ok((-1, Some(110)))`, meaning ETIMEDOUT. All original raw bytes and the failed result remain intact.

The enrollment helper and complete memory/four-method fixture are byte-identical between old source `696f0476aa46cf29e31b947a89379d80b4542ce3` and the current source. The helper first moves one waiter from the original word to a parking word, then requires a second syscall to move exactly one back. The waiter uses its original one-second FUTEX_WAIT timeout; the deliberate 150-millisecond delay precedes that syscall and does not consume its timeout. A successful forward requeue proves instantaneous enrollment, not continued enrollment across the separate reverse syscall.

The old failing assertion occurred before `start_pending_children` released this fixture's fatal worker. Initial parent VM setup had already executed, but the particular worker store/clear-TID wake/slot-release checks were never reached. That case therefore did not measure those backend assertions. The current four passes do reach their unchanged assertions: writable clearing, read-only failed store preserving the word, successful futex wake, reusable slot 0 and exact memory/status observations. They neither explain the old enrollment timeout nor establish a full-suite result.

No old-versus-new runtime comparison or full-suite-order reproduction was made. The new runs isolate individual methods on a newer library, so they cannot distinguish a historical scheduling delay, surrounding runtime behavior, full-suite state, or another cause. The narrower conclusion is that this first focused measurement did not reproduce the old enrollment failure.

## Next narrow diagnostic if pursued

Keep the original failing method, all four control identities, timeout values and every downstream assertion. Prepare a separately reviewed test-only diagnostic with fixed-size records for wait entry/return and errno; forward requeue entry/return (record first/last zero results and a count without unbounded logging); reverse entry/return; fatal-worker gate release; and clear-TID store/wake/slot-release events. Capture monotonic timestamps around each actual syscall, retain overflow as a diagnostic failure, and print the bounded record after the critical operations. Do not accept reverse zero, silently retry enrollment, extend a timeout, or inject success.

A wait-return ETIMEDOUT timestamp preceding reverse entry would establish timeout before that call. A later user-space return timestamp cannot order the earlier kernel timeout removal; such evidence must remain unresolved. Instrumentation can itself alter timing, so preserve the uninstrumented first results and do not infer that a passing instrumented attempt fixes the original failure. This paragraph is a follow-up design only: no diagnostic source change, build or execution is authorized or performed by this four-method task.

## Bindings

- Source binding: `043b859fea40ae2da1a95eab4774e57c1e72eeacf35eafbdc10534f31057119f`
- Actual library ELF: `daf7a52b631f19790dcac4e1d1fbbbd3326683ca3305a424db6befedb25cee8b`, 116212232 bytes, mode 0755
- Plan: `61dbe63af1022ae9ce4e26d6d53b5ffa399ce748dd41b403e6124ff2363e74c3`
- Caller: `f9cbfcf70516ad151f4f577263dec63f4bfe368c60d401d044b74626b7cb3dea`
- Actual launch: `f587d009affcacf3137546ae077ae53da9e75c484906deaa401d6a960cced6c3`
- Historical readback: `e2ac3344986a89a73e9cf030b5e4745e642e979fe29d9a6489e8e9ab32cc4c2c`
- Local preflight: `eaa8a896cc1f40e3eb1a18df9323b769538d85fe37184787cb5b1cd0efe04e91`
- RESULT.json: `01d4f0f9048308bd5f65e2fa116dc87531b92c108a7aac61e5c2a14851a990fa`
- Fresh terminal readback: `ad28c2dddb921d9004ef8d3ac6f0841ce0ef9cf1a55093703f46545480199388`

Full source and all 95 explicit inputs matched before and after execution; the actual library ELF still matches its retained copy. This is selected Reverie VM evidence, not Hermit strict INFO, repeat determinism, canonical parity or 100-percent backend completion.
