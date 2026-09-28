# First qualification: compiler failure preserved

The fresh qualification stopped at compile, raw 101 / accepted=false. Metadata passed. Exactly two phases were attempted; one passed and one failed. All 38 selected test declarations and the remaining 43 phases are unrun. No clock, timer, injection-memory, mode or legacy-vsyscall assertion was executed. No qualified test ELF was retained.

The compiler reports E0603 at injection_stop_tests.rs:31: the test imports PathPtr through the private args module. The public re-export exists at reverie-syscalls/src/lib.rs:35. It also reports the redundant std::io::Write import at line 7. Both are test imports; this receipt does not establish successful compilation of the complete candidate.

Compile payload elapsed 52.071816154 seconds; its observer elapsed 56.570059146 seconds and measured CPU was 106.060827000 seconds. Across metadata and compile, observer wall sums to 59.384566761 seconds and measured CPU to 107.984313000 seconds. Both services are inactive/dead with MainPID zero and empty control groups, with complete accounting.

The structured phase reader retains its original `missing successful Cargo completion` refusal. It stops before writing its diagnostic projection on an unsuccessful Cargo completion; this packet separately extracts the actual unchanged compiler-message records. Raw stdout/stderr remain authoritative. The driver returned 1 and did not retry.

Source, frozen inputs, assertions and original bounds remain unchanged. Normal private lease completion is authenticated; the last token remains recorded and an exclusive read-only lock probe succeeds after driver completion. The unrelated old C unresolved token was never opened. No failure is relabelled and no product, source, runtime, scheduler or parity approval is implied.
