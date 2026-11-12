# Same-inode replacement baseline result

The unchanged production code reproduced the intended stale-description failure on the first attempt. Both unchanged neighboring controls passed. The new test remains a failed test: raw status 101, `accepted=false`, with complete terminal accounting and unchanged bound inputs. This is a native executor unit measurement, not a VM, Hermit, FIFO, signal-delivery or parity result.

The exact production base is Reverie `000c15a1161ea2d58749431b5ddaaa97f7aa37d5`, tree `a12d56466eed51cdd7d12087b5fafa9d66d8c864`. The isolated baseline snapshot contains all 2,620 tracked entries and only the independently reviewed additive test, patch SHA256 `057f7702ff26f2c57de10e98d88a2e987a63736c76a43c613ac05460795d0ee9`. Cargo.lock remains separately bound local input, not a Git blob. The live product checkout, HEAD and index stayed clean and unchanged throughout every phase.

## Actual assertion

The exact selector `executor::tests::shared_file_table_reopen_same_inode_replaces_description` started once and failed once, with zero ignored or measured tests. It reached the final assertion at `reverie-kvm/src/executor.rs:25110:9`; all setup and both original-inode equality checks had completed before it.

```text
assertion `left == right` failed: same inode must not retain the replaced OFD; the dup keeps the original OFD
  left: (1, 32770, 1, [49], 2, 2, 32770)
 right: (5, 33794, 1, [53], 6, 1, 32770)
```

The fields are target offset before read, complete target status flags, read count, byte, target offset after read, retained dup offset, and complete dup status flags. Byte 49 is `1`; required byte 53 is `5`. The actual target remained on the old description at offset 1, without O_APPEND. Reading it moved both target and old dup to offset 2. The unchanged oracle instead requires the fresh description at offset 5 with O_APPEND, reading byte `5` to offset 6 while the old dup remains at offset 1 with its old flags. The observed tuple matches the earlier source prediction; no setup, compile or instrumentation refusal was substituted for it.

The original failing receipt is `controls/test-replaced-description/result.json`; its `accepted` field remains false. Raw libtest events and assertion output are retained in `observer/test-replaced-description/{stdout,stderr}`. `outcomes.json` records the actual failed event. The separate RESULTS.json authenticates the intended negative observation without altering or relabelling that failure as a passing test.

## Finite phases

| Phase | Raw status | Accepted | Payload seconds | Aggregate service CPU seconds |
| --- | --- | --- | --- | --- |
| Metadata | 0 | true | 0.1381 | 1.2735 |
| Library compile | 0 | true | 18.9035 | 30.6494 |
| Format | 0 | true | 0.2633 | 2.4178 |
| Actual library list | 0 | true | 0.0030 | 2.1729 |
| Existing shared-dup fdinfo control | 0 | true | 0.0036 | 2.1635 |
| Existing Persistent object-identity control | 0 | true | 0.0033 | 2.1760 |
| New same-inode replacement control | 101 | false | 0.0036 | 2.1553 |

All seven phases have `terminal_authenticated=true`, `inputs_unchanged=true`, complete observer accounting, and no stop reason or observer error. The list actually enumerated 489 declarations. Only the three prescribed test declarations were executed, once each: two passed and one failed. The other declarations were not tested. Compile produced no structured compiler diagnostics; format passed on the exact reviewed after file. There was no retry, source edit, assertion change, selector change or limit change.

The two positive exact selectors are `executor::tests::fdinfo_dispatch_retains_partial_records_and_shared_dup_seek_state` and `executor::tests::forked_states_share_file_object_identity_namespace`. Their scope remains the existing fdinfo dup/sequence behavior and Persistent filesystem-object identity respectively; they do not newly prove process-fork descriptor replacement or all epoll/lock behavior.

The original observer `137c9b42c3f9e081db59954e0ef692ed861f488be7c2bb8a15ef595c271b2179` and phase/common/lease helpers were copied unchanged. Metadata/compile retained 600 aggregate CPU seconds and 900 payload wall seconds. Format/list/each exact test retained 30 CPU seconds and 60 wall seconds. Each retained 16 GiB memory, no swap, 16 MiB lethal stderr, the maintained output guards and 100 GiB free-space floor. The compile's outer transport took 23.7810 seconds and the failed control's outer transport took 5.0394 seconds; those are distinct from the payload and cumulative service CPU values above.

## Source, executable and lifecycle binding

Fresh actual Cargo metadata contained 288 packages. Complete package metadata, resolved graph and workspace-member sets matched the original resolved metadata after only the declared isolated-source-root to owner-root substitution. All 7,083 external dependency files, totaling 170,540,804 bytes, were freshly authenticated; there were no dependency symlinks. The retained old closure record keeps its historical provenance, with the new comparison recorded separately in METADATA-CONTINUITY.json.

Cargo emitted one actual library test executable, 120,149,904 bytes, SHA256 `f1dd13829f04d940392e97704746b5544f04cf96974dfa7c69063e6635ca5a25`. It is retained on a distinct inode with link count 1 at `retained/reverie-kvm-lib`. ELF-RETENTION.json binds the original Cargo artifact, source manifest, compile receipt and retained copy. Subsequent phases used the same authenticated artifact with the dated nightly toolchain and actual loader closure. No historical executable was reattributed to this test source.

After the final phase, all 2,620 snapshot entries and the local lock were verified unchanged. The live source, branch, HEAD and index matched the initial clean state. The lease's terminal completion points to the unchanged failed receipt. I checked that an exclusive nonblocking hold was available on the same lease inode/token, authenticated terminal/empty service state from the completed phase, and closed that hold without changing the token or completion. LEASE-RELEASE.json records that readback. Root and the source author were notified that no source or target hold remains; future live edits do not alter the frozen snapshot or retained executable.

A later report-construction command had a Python syntax error before executing any statement. REPORT-BUILD-REFUSAL.json preserves that reporting-only failure. It did not run a product/test, change an original result, or prompt a test retry. The corrected report construction read the retained receipts only.

RESULTS.json binds 97 evidence records and the exact limits/outcomes. This packet establishes the source-reachable stale-OFD defect in the stated production component. It does not attribute it to Reverie pull request 538, the earlier census, or timer behavior. A future repair still needs source review and qualification that preserves stable same-description host fds, dup/fork semantics, filesystem-object identity and existing epoll/lock obligations.
