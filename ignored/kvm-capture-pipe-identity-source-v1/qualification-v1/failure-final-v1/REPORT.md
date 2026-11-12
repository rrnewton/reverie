# Captured pipe identity: first qualification failure

Metadata passed; the fresh compile failed raw 101. The caller stopped without retry. Two phases ran, one passed and one failed. All 63 tests and the seven remaining non-test phases are unrun. No test ELF was qualified or retained, no isolated child completion marker exists, and no runtime/metadata/parity claim follows from the partial compile.

The new GlobalTool implementation in capture_identity_tests.rs:362 omits required receive_rpc (E0046). An existing executor.rs:39601 readlink_at_impl call still passes false after its parameter changed to Option<CaptureMetadata> (E0308). The complete compiler JSON, rendered diagnostics and unsuccessful Cargo completion remain bound. No assertion or compiler gate was relaxed.

The compile payload ran 41.397573 seconds, observer 44.906687 seconds, and used 86.086099 CPU seconds. Including metadata, the two observed phases used 87.699640 CPU seconds and 47.584012 summed observer-wall seconds. All original 600 CPU / 900 wall compile/metadata allowances, output/memory/free-space guards, source bytes, caller bytes and SCM identity remain unchanged.

The private lease last token names this actual failed compile and its authenticated append-only terminal completion. A fresh nonblocking exclusive lock succeeded; both retained service units were independently queried inactive/empty with MainPID zero. The audit closed its hold without rewriting the token or performing recovery. This is terminal lifecycle evidence for a real failure, not successful build accounting.

The source carrier remains the complete frozen B source plus the identical V6 lock. The original source/report/caller, prior missing-lock and static-freeze refusals, stdout/pipe witness, old V6 evidence and teardown hangs remain unchanged. The existing private build target is retained as failed-build residue, not indexed as immutable runtime evidence. Raw Cargo artifact paths are historical output records; none were executed or credited. A separately frozen source successor and caller authorization are required before another qualification attempt.
