# Unsupported GDB vFile repair

Ready for exact-source review: Reverie f83b6d8f45b5092676d9ffa1d99a742660eac661 → 17982ed4903b8097f6e0451fc02338595c89f3d7, tree 9060a20530863fbc69991722a60cfb02749622a2. The narrow protocol regression is reproduced on old production and passes with the repair. No Hermit guest retry, full-workspace pass, publication or main-green result is claimed.

## Source and causal boundary

The GNU GDB Host-I/O protocol says to compare the complete operation name through the second colon and return an empty reply for an unrecognized operation. Official source and its successful proxy fetch are retained as gdb-host-io-protocol.html, .txt and -fetch.json. URL: https://sourceware.org/gdb/current/onlinedocs/gdb.html/Host-I_002fO-Packets.html .

At the base, Command::try_parse recognizes the broad vFile prefix, then _vFile.rs returns None for lstat. That becomes MalformedCommand; Packet::new propagates it, the real server relay drops its command sender and the joined Session ends, closing the connection. The retained Hermit CLI2 protocol trace ended at vFile:lstat and then reported remote closure and an internal GDB crash before its Python injection. This establishes the unsupported-request disconnection; the new native result does not establish a completed Hermit CLI test or every debugger cleanup path.

Only three files change, +173/-1: commands/base/_vFile.rs adds an Unsupported variant and a fallback requiring a nonempty unknown name and both colon delimiters; session.rs writes the existing empty response for that variant; server.rs adds only test code. Every existing supported branch returns through its original argument parser, including malformed errors. There is no lstat implementation, host path access, invented F success, global parse-error normalization, packet-size change or checksum relaxation. The existing empty-path hex decoder can panic through len()-1; that pre-existing limitation was found in source and is unchanged. No graceful-error guarantee is made for that case.

committed.patch SHA256 f2885cec0ac104a6381d5f0d7e2188ca850b2251b62de5c9fb20f137b8c3d922 is byte-identical to candidate-1/actual-git.patch and binds the same three full candidate files reviewed before execution. COMMITTED.json and final-tree.json bind all 2528 recursive tree entries: all other entries/modes are identical to the base. The tracked checkout is clean; its own HANDOFF.md and ignored evidence tree remain untracked. Normal explicit-path commit completed with exit0 in0.199s. Installed composite Git hooks ran without bypass; this Reverie tree contains no tracked .githooks implementations. The exact degraded-dev-hermit attribution output is retained and copied verbatim, including the resolver limitation.

## Meaningful old/new execution

Four added tests execute the actual parser and the real framed duplex connection through GdbServerImpl::run, relay, Packet parsing and Session::run. Initial stopped-inferior data is synthetic and inert: no guest, ptrace operation, signal or external debugger runs. Positive packets include lstat and unknown names sharing a supported prefix. Each must return exact ACK plus empty frame (+$#00), then the same connection must respond to ! with +$OK#9a. Separate connections must still close for malformed open and pread. Parser controls cover missing separators and arguments and invalid hex for all eight supported operations. The complete existing GDB-stub test population is included, preserving checksum, frame fragmentation/coalescing, binary escapes, packet-size and disconnect controls.

The same entire added test sections were run on both variants. Separate owned targets and Cargo JSON executable/manifests prevent a stale binary from masquerading as a source result. The repository does not track Cargo.lock: old ordinary Cargo generated it; the new compile used that exact lock with --locked --offline. Its SHA256 is537569ebbb0b298f6a386f4e5cc57fe2d3c33280b470752b519a410a1ef32675 and retained as resolved-Cargo.lock. Both use the repository's nightly-2026-07-29 toolchain and existing CI link argument -llzma.

| Variant/check | Actual result | Time |
| --- | --- | --- |
| Old production plus new tests, compile | exit0 |37.486s |
| Old complete GDB-stub selection | exit101;37pass,2intendedfail |0.103s |
| New independent-target compile | exit0 |34.396s |
| New complete GDB-stub selection | exit0;39pass,0fail,0ignored |0.120s |
| Targeted reverie-ptrace library/tests Clippy, -D warnings | exit0 |16.132s |
| Formatting of all three changed files | exit0 |0.029s |

Old failures are actual test assertions: lstat is rejected by the parser and the live stream read yields UnexpectedEof. Compilation succeeded. Both malformed controls and all35 original GDB-stub tests pass on both variants. The other138 library tests were outside this focused selection, not claimed passed. Old/new inventories are byte-identical. Executable hashes: old b4d7a2eb3f6a6134539886b49e8933543f3124e76fa8440694fbb3bdd4451552; new75aadc155c801e81b5428ab817cdf2fc705ed76e6fc872f13b97b042b9c1fdfa. Source snapshots before and after each run match; the final commit's files equal the tested candidate bytes.

## Bounds, identity and retained failures

Both native runs used4CPU/8GiB/zero swap/1024tasks/900s, CARGO_BUILD_JOBS=4, private Cargo home and separate target directories. The only cache seed came from our terminal CLI checkout; registry archive/index and Git bytes were copied independently with before/after equality and no shared inode. No foreign writable cache or prepared-result authority was used.

Old exec6887 returned101, including19.695s initial private cache preparation and37.486s compile; launcher elapsed58.187s. Scope reverie-gdb-vfile-old-1-20260916.scope, Invocation2cd8abb777ab4c9a841cc7293b791ea9, controller PID3709865/start175783078, cgroup inode37988812. Peak memory4103639040B, peak61tasks, memory/pids events all0.

New exec15189 returned0. Scope reverie-gdb-vfile-new-1-20260916.scope, Invocation6c957e665c5b446590027b9b1386b252, PID3858736/start175791742, cgroup inode37991213. Peak memory2447527936B, peak58tasks, memory/pids events all0. Command durations are retained individually; a separate exact outer elapsed duration was not captured. Both original controller PIDs and cgroups are absent, and both units read back not-found. These are component measurements under concurrent host activity, not a comparative performance claim.

preparation-attempts.json retains the wrong wrapper-path refusal and systemd-service ancestry refusal before successful canonical scope-based slot creation. tests-draft-1.patch retains the corrected, unexecuted assumption that every empty path argument would return an error rather than reaching the old hex-decoder panic. No test failure was converted into a pass by changing the implemented oracle. who-am-i's initial JSON/role CLI refusal is retained. The old intended red result is preserved, not relabelled green.

## Goalpost-moving assessment

- Assertions weakened: no. All pre-existing test bodies and strict frame/error checks remain; the same new assertions discriminate old and repaired production.
- Tolerance widened, exemption added, case skipped or comparator relaxed: no. No existing bounds, selection, ignored identity, packet size or comparator changed. Unknown complete operations intentionally acquire the protocol-required unsupported response; known malformed operations do not acquire an exemption.
- Failure renamed or relabelled as a pass: no. Old37/2 and the separate failed Hermit CLI run remain failures. An empty reply means unsupported, not successful host I/O, and no guest result is credited.
- Check deleted instead of satisfied: no. Packet validation and known-operation parser checks remain; the unsupported negotiation is now handled without dropping the session.

This is the author's source/evidence packet, not an independent reviewer attestation. Root has reviewed the artifact source; exact final-head approval and any publication remain with root.
