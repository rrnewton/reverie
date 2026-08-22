# Netlink in detcore: where we start, and where we need to get to

Agent: `netlink` | Task: `netlink-where-we-start-and-where-we-need-to-get-to` | 2026-08-20

Repo state for every line number and link below:

```
$ git -C hermit rev-parse HEAD origin/main
fda2b0b2874fead40171ab7ffa4aa7cfd638c8ea
fda2b0b2874fead40171ab7ffa4aa7cfd638c8ea
$ git -C hermit rev-list --left-right --count origin/main...HEAD
0	0
```

HEAD *is* `origin/main`, and `git status --porcelain` is empty for all five files
below, so the links are the code as it stands on main. The remote agrees, so
every link below resolves:

```
$ with-proxy git ls-remote origin refs/heads/main
fda2b0b2874fead40171ab7ffa4aa7cfd638c8ea	refs/heads/main
$ git remote get-url origin
https://github.com/rrnewton/hermit.git
```

Measurements were made with `hermit/target/release/hermit`, built at
`770b95c505fa`. That is 5 commits behind HEAD, so I checked it is still a valid
binary for this subject:

```
$ git diff --stat 770b95c505fa..HEAD -- detcore/src/sock_diag.rs \
    detcore/src/syscalls/files.rs detcore/src/syscalls/io.rs \
    detcore/src/fd.rs detcore/src/procfs.rs
(no output)
```

No netlink-handling code changed between the binary and HEAD.

---

## 0. The link you asked for

There is no file called `netlink.rs`. Netlink handling in detcore is **five
files, 5 code sites, roughly 300 lines of logic**. Here is each one and what it
does.

| # | file | what it does |
|---|---|---|
| 1 | [`detcore/src/syscalls/files.rs`](https://github.com/rrnewton/hermit/blob/main/detcore/src/syscalls/files.rs) | recognises the protocol at `socket()` and rewrites the socket's own identity at `bind()` |
| 2 | [`detcore/src/syscalls/io.rs`](https://github.com/rrnewton/hermit/blob/main/detcore/src/syscalls/io.rs) | the **only** place a netlink *reply payload* is ever rewritten |
| 3 | [`detcore/src/sock_diag.rs`](https://github.com/rrnewton/hermit/blob/main/detcore/src/sock_diag.rs) | the one reply-content normaliser (403 lines: 221 code, 182 unit test) |
| 4 | [`detcore/src/fd.rs`](https://github.com/rrnewton/hermit/blob/main/detcore/src/fd.rs) | one boolean on the open-file description, carrying "this is the protocol we normalise" from `socket()` to `recvmsg()` |
| 5 | [`detcore/src/procfs.rs`](https://github.com/rrnewton/hermit/blob/main/detcore/src/procfs.rs) | sanitises the *text* view of the netlink socket table, `/proc/net/netlink` — not netlink traffic itself |

Exact-line permalinks, pinned at `fda2b0b2874f` so they cannot rot:

**1. `files.rs` — protocol recognition and socket identity**
- [`files.rs#L1963-L1973`](https://github.com/rrnewton/hermit/blob/fda2b0b2874fead40171ab7ffa4aa7cfd638c8ea/detcore/src/syscalls/files.rs#L1963-L1973) — `mark_sock_diag_fd`. **This is the only line in detcore that tests a netlink protocol number.**
- [`files.rs#L2216-L2245`](https://github.com/rrnewton/hermit/blob/fda2b0b2874fead40171ab7ffa4aa7cfd638c8ea/detcore/src/syscalls/files.rs#L2216-L2245) — the `bind()` arm that replaces a kernel-assigned socket identity with a deterministic one.
- [`files.rs#L106-L107`](https://github.com/rrnewton/hermit/blob/fda2b0b2874fead40171ab7ffa4aa7cfd638c8ea/detcore/src/syscalls/files.rs#L106-L107) — the base constant `0x4000_0000` those identities are built from.

**2. `io.rs` — the only reply-content intervention**
- [`io.rs#L1319-L1329`](https://github.com/rrnewton/hermit/blob/fda2b0b2874fead40171ab7ffa4aa7cfd638c8ea/detcore/src/syscalls/io.rs#L1319-L1329) — the diversion at the top of `handle_recvmsg`.
- [`io.rs#L1371-L1449`](https://github.com/rrnewton/hermit/blob/fda2b0b2874fead40171ab7ffa4aa7cfd638c8ea/detcore/src/syscalls/io.rs#L1371-L1449) — `handle_sock_diag_recvmsg`, which gathers the scatter-gather buffers, calls the normaliser, and writes back preserving segment boundaries.

**3. `sock_diag.rs` — the normaliser**
- [`sock_diag.rs`, whole file](https://github.com/rrnewton/hermit/blob/fda2b0b2874fead40171ab7ffa4aa7cfd638c8ea/detcore/src/sock_diag.rs)
- [`#L60`](https://github.com/rrnewton/hermit/blob/fda2b0b2874fead40171ab7ffa4aa7cfd638c8ea/detcore/src/sock_diag.rs#L60) the single public function; [`#L176-L191`](https://github.com/rrnewton/hermit/blob/fda2b0b2874fead40171ab7ffa4aa7cfd638c8ea/detcore/src/sock_diag.rs#L176-L191) the family dispatch; [`#L118-L159`](https://github.com/rrnewton/hermit/blob/fda2b0b2874fead40171ab7ffa4aa7cfd638c8ea/detcore/src/sock_diag.rs#L118-L159) the zeroing and message-order canonicalisation.

**4. `fd.rs` — the flag**
- [`fd.rs#L128-L131`](https://github.com/rrnewton/hermit/blob/fda2b0b2874fead40171ab7ffa4aa7cfd638c8ea/detcore/src/fd.rs#L128-L131), [`#L435-L446`](https://github.com/rrnewton/hermit/blob/fda2b0b2874fead40171ab7ffa4aa7cfd638c8ea/detcore/src/fd.rs#L435-L446)

**5. `procfs.rs` — the text view, not the wire**
- [`procfs.rs#L496`](https://github.com/rrnewton/hermit/blob/fda2b0b2874fead40171ab7ffa4aa7cfd638c8ea/detcore/src/procfs.rs#L496) routes `/proc/net/netlink`; [`#L1863-L1925`](https://github.com/rrnewton/hermit/blob/fda2b0b2874fead40171ab7ffa4aa7cfd638c8ea/detcore/src/procfs.rs#L1863-L1925) is the sanitiser.

### One correction you asked me to carry, and its status

Netlink is not process-to-process IPC. It is a socket-shaped interface from a
process to the **kernel**: `socket(AF_NETLINK, …)`, `sendmsg()` a request,
`recvmsg()` a reply from a kernel subsystem. Nothing leaves the machine and no
second process is involved.

**detcore's own code does not make this mistake.** I grepped every netlink
mention in `detcore/` and `hermit-cli/` for the misreading and found nothing:

```
$ grep -rn -i "netlink" --include=*.rs detcore/ hermit-cli/ \
    | grep -i "ipc\|inter-process\|interprocess\|process-to-process\|another process\|peer process"
(no output)
```

The docstrings describe it correctly, e.g. `sock_diag.rs:10-12`: *"Tools such as
`ss` enumerate sockets over an `AF_NETLINK`/`NETLINK_SOCK_DIAG` socket and
receive a binary, multipart `nlmsghdr`-framed reply."* Nothing to fix here.

---

## 1. How much does detcore support this today?

Not a narrative. Here is the surface, as a grid.

### 1.1 By protocol

The protocol is the third argument to `socket()`. Linux names 22 of them —
`NETLINK_ROUTE` = 0 through `NETLINK_SMC` = 22, with `MAX_LINKS` = 32 as the
ceiling (`/usr/include/linux/netlink.h:9-35`). detcore tests exactly **one**
number, at `files.rs:1968`:

```rust
if call.family() == libc::AF_NETLINK && call.protocol() == libc::NETLINK_SOCK_DIAG {
```

```
$ grep -rn "NETLINK_" --include=*.rs detcore/src/ | grep -v sock_diag.rs
detcore/src/fd.rs:128,435,442               (comments)
detcore/src/syscalls/io.rs:1319,1371        (comments)
detcore/src/syscalls/files.rs:107           DETERMINISTIC_NETLINK_PORT_ID_BASE
detcore/src/syscalls/files.rs:1963          (comment)
detcore/src/syscalls/files.rs:1968          libc::NETLINK_SOCK_DIAG   <-- the only test
detcore/src/syscalls/files.rs:2229          (uses the base constant)
```

| protocol | number | recognised? | what happens to its replies |
|---|---|---|---|
| `NETLINK_SOCK_DIAG` | 4 | **yes**, `files.rs:1968` | socket flagged; `recvmsg()` replies rewritten by `sock_diag.rs` |
| `NETLINK_ROUTE` | 0 | no | reply copied to the guest byte-for-byte as the kernel produced it |
| `NETLINK_USERSOCK` | 2 | no | same |
| `NETLINK_GENERIC` | 16 | no | same |
| every other (`NETLINK_AUDIT`, `NETLINK_XFRM`, `NETLINK_KOBJECT_UEVENT`, …) | — | no | same |

Unrecognised protocols are **not blocked, not warned about, and not logged as
unsupported**. `socket()` succeeds and the reply passes through untouched. From
detcore's point of view an `RTM_GETLINK` dump is an opaque byte buffer.

### 1.2 By syscall

| syscall | what detcore does for netlink | where |
|---|---|---|
| `socket()` | records the fd; if protocol == 4, sets one boolean. Otherwise nothing netlink-specific. | `files.rs:1918-1958`, `:1967-1973` |
| `bind()` | if `nl_pid == 0` (the guest asking the kernel to pick a name), substitutes `0x4000_0000 \| port` from detcore's own port allocator. Applies to **all** netlink protocols. | `files.rs:2216-2245` |
| `getsockname()` | nothing netlink-specific — but it returns the identity `bind()` already substituted, so it is deterministic as a consequence. | `files.rs:2044-2050` |
| `sendto()` / `sendmsg()` / `sendmmsg()` | nothing netlink-specific. Correct: requests are guest-authored, so they are already deterministic. | `lib.rs:2298-2300` |
| `recvmsg()` | **the one intervention.** If the fd is flagged **and** `cfg.virtualize_metadata` is set, divert to `handle_sock_diag_recvmsg`. The metadata flag defaults true (`detcore/src/config.rs:181`) and is forced false only under `--strace-only` (`run.rs:2155`), so in practice the gate is the flag. | `io.rs:1319-1329` |
| `recvfrom()`, `recvmmsg()`, `read()`, `readv()`, `preadv()` | **nothing.** None of these consult the flag. | `lib.rs:2294, 2310, 1972, 1984, 1985` |
| `setsockopt(SOL_NETLINK, …)` | **nothing.** No code anywhere. Multicast group subscription is unhandled. | — |

The flag is read at exactly one production site:

```
$ grep -rn "is_sock_diag" --include=*.rs detcore/ hermit-cli/
detcore/src/syscalls/io.rs:1325   <-- production, the only one
detcore/src/fd.rs:444             (definition)
detcore/src/fd.rs:504,507         (unit test)
```

### 1.3 What `sock_diag.rs` actually covers

`zero_family_inodes` (`sock_diag.rs:176-191`) dispatches on the address family
in the first byte of each message body:

```rust
match family {
    libc::AF_UNIX    => { /* zero udiag_ino at +4; zero UNIX_DIAG_PEER attrs */ }
    libc::AF_NETLINK => { /* zero ndiag_ino at +16 */ }
    _ => {}
}
```

Two families covered. **`AF_INET` and `AF_INET6` — `struct inet_diag_msg`, whose
`idiag_inode` is a host-assigned inode number exactly like the other two — fall
into `_ => {}` and are left untouched.** That is what `ss -t`, `ss -u`, and
`ss -a`'s TCP/UDP rows return.

The same hole exists on the text side. `procfs.rs` sanitises `/proc/net/netlink`
(`:496`) and `/proc/net/unix` (`:502`); there is no `/proc/net/tcp`,
`/proc/net/tcp6`, `/proc/net/udp` or `/proc/net/udp6` arm at all. So the two
paths are at least *consistent with each other*, which is what the module
docstring promises (`sock_diag.rs:19-22`) — they are consistently incomplete.

Besides zeroing inodes, the module also **sorts the diag messages** by byte
content (`sock_diag.rs:118-159`), but only when they form a contiguous prefix
followed by a non-diag trailer; any other shape is left in original order. This
mirrors the row-sort the procfs text sanitisers apply.

---

## 2. `detcore/src/sock_diag.rs` — provenance, coverage, and whether anything tests it

**Provenance.** Two commits, both on main, neither human-reviewed:

```
$ git log --oneline -- detcore/src/sock_diag.rs
4934947922 detcore/sock_diag: fix portable-gate clippy errors
f5e16c3303 detcore: determinize NETLINK_SOCK_DIAG dump inodes and message order

$ git log -1 --format='%H %ci %s' f5e16c3303
f5e16c3303bb5df0b19202f7367535c088562d09 2026-07-28 07:05:43 -0400 detcore: determinize NETLINK_SOCK_DIAG dump inodes and message order
$ git merge-base --is-ancestor f5e16c3303 origin/main && echo YES
YES
```

`f5e16c3303` introduced the file at 401 lines; `4934947922` is a lint fix
bringing it to its present 403. All the behaviour is from the first commit.

The file carries `AUTONOMOUS-BOT-IMPLEMENTED` / `TODO-HUMAN-REVIEW(PR-1064)` at
`sock_diag.rs:30-31`, and the two call sites carry the same marker
(`files.rs:1961-1962`, `io.rs:1369-1370`).

**Design properties worth keeping.** The docstring at `sock_diag.rs:24-28` states
the parser is pure and fail-open, and only ever zeroes fixed-width fields in
place — never grows, shrinks, or shifts the buffer, so a partial parse cannot
corrupt message boundaries. That shape is right and any future normaliser should
copy it.

### Does anything test it? **Its zeroing logic has zero coverage on real bytes.**

This is a new finding, it is measured, and I had to correct it once during this
investigation — the first version of this section was too strong. Here is the
accurate version.

The normaliser is reached only from `io.rs:1436`. To reach that, a test must run
a guest that opens `socket(AF_NETLINK, …, NETLINK_SOCK_DIAG)` and calls
`recvmsg()` on it. Two places in the repo run a program that might:

**(a) `hermit-cli/tests/sockstat_determinism.rs:156-160` — does not reach it at all.**

```rust
ProgramCase {
    name: "ss summary",
    candidates: &["/usr/sbin/ss", "/usr/bin/ss"],
    args: &["-s"],
},
```

`ss -s` does not use netlink. Measured:

```
$ ./target/release/hermit --log=info run -- /usr/sbin/ss -s 2>&1 >/dev/null \
    | grep "inbound syscall" | sed -E 's/.*inbound syscall: //;s/\(.*//' | sort | uniq -c | sort -rn
     47 mmap
     16 read
     15 fstat
     14 openat
     14 close
     13 mprotect
      6 prctl
      4 pread64
      3 brk
      2 statfs
      2 lseek
      2 arch_prctl
      2 access
      1 write
      ... (21-22 distinct syscalls, ~148 total; an independent rerun got 22/149.
          The counts wobble by one; the zero does not.)
```

**Zero `socket()` calls.** `ss -s` reads `/proc/net/sockstat` as text. The flag
at `files.rs:1968` never fires, `io.rs:1325` never returns true, and
`sock_diag.rs` is never entered.

The flags that *would* carry a populated dump:

```
$ for a in "-s" "-a" "-x -a" "-t -a"; do
    echo "ss $a:"; ./target/release/hermit --log=info run -- /usr/sbin/ss $a 2>&1 >/dev/null \
      | grep "inbound syscall" | grep -c "socket(16"; done
ss -s:      0
ss -a:     11
ss -x -a:   1
ss -t -a:   1
```

`socket(16, 524291, 4)` = `AF_NETLINK`, `SOCK_RAW|SOCK_CLOEXEC|SOCK_NONBLOCK`,
protocol 4.

**(b) The compat corpus `ss` cell — reaches the wrapper, but only on empty replies.**

`ci/compat/corpus-strict.json:828` and `corpus-sabre.json:808` both dispatch to
`tests/compat/real_compat_workload.sh:503-512`, which runs:

```bash
output=$("$SS" -H -ltn 'sport = :0')
test -z "$output"
printf 'ss:no-port-zero-listener\n'
```

This *does* open a protocol-4 socket, so `io.rs:1325` returns true and
`handle_sock_diag_recvmsg` is entered. But measure what it receives:

```
$ ./target/release/hermit --log=info run -- /usr/sbin/ss -H -ltn 'sport = :0' 2>&1 >/dev/null \
    | grep "finish syscall" | sed -E 's/.*finish syscall #[0-9]+: //' | grep -E "socket\(16|recvmsg|sendmsg"
socket(16, 524291, 4) = Ok(3)
sendmsg(3, 0x7fffffffb0e0, 0) = Ok(92)
recvmsg(3, 0x7fffffffaf20, 34) = Ok(20)
recvmsg(3, 0x7fffffffaf20, 0) = Ok(20)
sendmsg(3, 0x7fffffffb0e0, 0) = Ok(92)
recvmsg(3, 0x7fffffffaf20, 34) = Ok(20)
recvmsg(3, 0x7fffffffaf20, 0) = Ok(20)
```

**Every reply is 20 bytes** — a bare `NLMSG_DONE` (16-byte `nlmsghdr` + 4-byte
payload) and *zero* `SOCK_DIAG_BY_FAMILY` messages. Inside
`sanitize_sock_diag_inodes`, `diag_count` is 0, nothing is zeroed, `modified`
stays false, and the function returns false leaving the buffer untouched
(`sock_diag.rs:118-163`). Two further reasons this cell could not exercise the
zeroing even against a non-empty result: `-t` restricts the query to TCP, i.e.
the `AF_INET`/`AF_INET6` families `zero_family_inodes` does not handle (§1.3);
and the assertion is `test -z "$output"`, which *asserts the result set is
empty*.

**No other guest in the repo issues a populated dump.** Stating this needs a
search over shell/yaml/json, not just Rust — the compat cell above was missed
exactly because an earlier version of this document searched for the *symbol*
`sock_diag` in `*.rs`/`*.c` only. Searching for the *program*:

```
$ grep -rn "sbin/ss\|bin/ss\"\|\"ss\"\|\bss -" --include=*.rs --include=*.yaml \
    --include=*.json --include=*.sh --include=*.py . \
  | grep -v scratch | grep -v ignored | grep -v /target/
ci/compat/corpus-strict.json:828         label "ss"        -> the cell dissected above
ci/compat/corpus-sabre.json:808          label "ss"        -> same cell
hermit-cli/tests/sockstat_determinism.rs:158                -> ss -s, no netlink
scripts/lib/validate_corpus.rs:268       "ss" in a label list
tests/compat/real_compat_workload.sh:504                    -> the cell above
tests/debugger/harness.py:99-105         ss -ltn
```

The last one is the only unfiltered `ss` in the tree, and it does **not** run
under hermit — it is a host-side readiness probe polling for a gdbserver listen
port. So no test anywhere receives a populated `SOCK_DIAG_BY_FAMILY` reply inside
a guest.

**Net position.** The gather/scatter wrapper in `io.rs:1378-1449` is entered by
one compat-corpus cell, on 20-byte replies containing nothing to sanitise. The
zeroing and message-ordering logic in `sock_diag.rs` has never run against a real
kernel reply under any test in this repository. Its only tests are the 182 lines
of unit test inside the module itself (`sock_diag.rs:222-403`) on hand-built
synthetic buffers — good unit tests, covering both handled families, the peer
attribute, message reordering, and five fail-open cases, but entirely
disconnected from the wiring.

### The rest of the netlink test surface

| test | what it actually covers | in blocking CI? |
|---|---|---|
| `detcore/tests/misc/mod.rs:929` `netlink_autobind_port_ids_are_deterministic` | binds two sockets each for `NETLINK_ROUTE`, `NETLINK_USERSOCK`, `NETLINK_GENERIC`; asserts `nl_pid != 0`, `nl_groups == 0`, and the two differ; prints them (`:980`). **It does compare across runs** — the harness runs the body twice and byte-compares stdout (see below). | yes |
| `tests/c/netlink_autobind_{route,usersock,generic}.c` | same probe as a C guest, printing the port IDs to stdout, so `--verify` *does* compare them across runs | **no** — inventory says `ci=false`, "remains outside blocking CI until its standalone build and output contract are calibrated" |
| `hermit-cli/tests/netlink_table_determinism.rs` | `/proc/net/netlink` **text** under `cat`/`awk`/`sed` with `--verify`; asserts pointer column and inode column are zeroed | yes |
| `hermit-cli/tests/sockstat_determinism.rs` | `/proc/net/sockstat` **text**; the `ss -s` case makes no netlink calls (above) | yes |
| compat corpus cell `ss` (`ci/compat/corpus-strict.json:828`, `corpus-sabre.json:808` → `tests/compat/real_compat_workload.sh:503-512`) | the only thing in the repo that opens a protocol-4 socket. Runs `ss -H -ltn 'sport = :0'`; every reply is a 20-byte `NLMSG_DONE`, and the assertion is that the output is empty | compat matrix |
| `third-party/rr/src/test/netlink_mmap_disable.c` via `hermit-cli/tests/rr_suite.rs:368` | an rr regression test that netlink mmap-ring mode is refused; not a determinism test | yes |

**A correction I owe this table.** I first wrote that
`netlink_autobind_port_ids_are_deterministic` was a single run "despite the
name". That was wrong, and an adversarial check caught it. The cross-run
comparison is real but three hops away from the test body:
`det_test_fn_sequential_without_pmu` (`detcore/tests/misc/mod.rs:61`) →
`det_test_fn_with_config` (`detcore/tests/testutils/src/lib.rs:579`) →
`det_test_fn_with_config_repetitions` with `TEST_REPS = 3`
(`testutils/src/lib.rs:33`), i.e. two repetitions. Each repetition is a separate
forked guest whose stdout is captured
(`reverie/reverie-ptrace/src/tracer.rs:2468`, `dup2_stdout` at `:2501`), and
`check_output` (`testutils/src/lib.rs:664`) asserts at `:692-699` that
`last_stdout == stdout_str` — *"Consecutive runs of test had different stdout"*.
Since the body prints the port IDs at `:980`, they are byte-compared across runs.
The netlink socket identity is genuinely covered in blocking CI.

Summary: **every netlink test in blocking CI tests either the socket's own
identity or a `/proc/net/*` text file. Not one tests the content of a netlink
reply.**

---

## 3. The measured defect

Re-measured independently for this write-up.

`python3 -c pass` opens two `NETLINK_ROUTE` sockets before user code runs, and
issues four dumps:

```
$ ./target/release/hermit --log=info run -- python3 -c pass 2>&1 >/dev/null \
    | grep "finish syscall" | grep -E "socket\(16|bind|sendto|recvmsg|getsockname" \
    | sed -E 's/.*finish syscall #[0-9]+: //'
socket(16, 524291, 0) = Ok(11)          <- protocol 0 = NETLINK_ROUTE
bind(11, 0x7fffffff86c4, 12) = Ok(0)
getsockname(11, 0x7fffffff86c4, 0x7fffffff86c0) = Ok(0)
sendto(11, 0x7fffffff8670, 20, 0, 0x7fffffff8654, 12) = Ok(20)
recvmsg(11, 0x7fffffff8670, 0) = Ok(1468)
recvmsg(11, 0x7fffffff8670, 0) = Ok(20)
sendto(11, 0x7fffffff8670, 20, 0, 0x7fffffff8654, 12) = Ok(20)
recvmsg(11, 0x7fffffff8670, 0) = Ok(156)
recvmsg(11, 0x7fffffff8670, 0) = Ok(20)
... the same sequence again on a second socket
```

That is glibc's `getifaddrs()`. Note `bind()` **is** called, so the identity
substitution at `files.rs:2218` does fire — the socket's own name is
determinized. Only the reply content is not.

Six runs of an `RTM_GETLINK` dump under hermit, default settings:

```
$ for i in 1 2 3 4 5 6; do ./target/release/hermit run --strict -- \
    python3 experiments/netlink-leak_20260820/netlink_probe.py --hex 2>/dev/null \
    | tail -1 > /tmp/nl_$i.hex; done
```

| measure | value |
|---|---|
| reply length | 1488 bytes, identical in all 6 runs |
| distinct sha256 | **6 of 6** |
| byte positions that differ | **4**: 772, 773, 776, 777 |

Decoding rather than diffing blind — the `rtattr` header at offset 764 is
`(len 20, type 5)`, i.e. `IFLA_INET6_CACHEINFO`, so its 16-byte payload is
768..784 and decodes as `struct ifla_cacheinfo`:

```
$ python3 -c "import struct; b=bytes.fromhex(open('/tmp/nl_1.hex').read().strip()); \
    print(struct.unpack('<IIII', b[768:784]))"
(65535, 63108078, 36728, 1000)
   ^max_reasm_len  ^tstamp  ^reachable_time  ^retrans_time
```

**Both middle fields vary. Two named fields, not one.**

- `reachable_time`, offset 776. Six samples: 36728, 32030, 25798, 21039, 32777,
  23115 — 6 of 6 inside [15000, 45000], which is the RFC 4861 §6.3.2 randomised
  range for the configured base (`/proc/sys/net/ipv6/neigh/lo/base_reachable_time_ms`
  = 30000). Consistent with the established finding; I am not re-deriving it.
- `tstamp`, offset 772. **This is not randomness — it is the host clock.**
  `/proc/uptime` = 631111.75 s and `getconf CLK_TCK` = 100, so host uptime in
  `USER_HZ` is 63,111,175; the measured `tstamp` is 63,108,078, 31 s earlier —
  the moment the interface was last updated. Across the six runs it climbs
  monotonically: 63108078, 63108139, 63108179, 63108220, 63108261, 63108327.

**A correction to carry forward: "4 bytes" is an artifact of the sampling
window, not a property of the leak.** Only the *low halves* of these two `u32`s
took different values in a run sequence spanning a few seconds on a host with
7.3 days of uptime. Two `u32` fields are at risk, i.e. **8 bytes**, and the high
halves will vary as soon as the host uptime or the random draw crosses a
16-bit boundary. Any fix must treat both fields, all 8 bytes.

The two differ in kind, and that matters for the fix:

- `reachable_time` is a **random draw**. Two runs *may* coincide.
- `tstamp` is a **monotone host clock**. Two runs at different wall-clock times
  can essentially never coincide. It is the guaranteed divergence.

---

## 4. Exactly where `--verify` is blind — measured, and more precise than "it reports parity"

`hermit run --verify` has **two independent comparators**, and only one of them
is general-purpose.

**The airtight demonstration first.** Run `--verify` on a probe that hashes the
reply to stdout. In *one and the same* verification pair, the two comparators
disagree:

```
$ ./target/release/hermit run --strict --verify -- python3 .../netlink_probe.py
Mismatch in stdout between run 1 and run 2:
  total_bytes 1488
- sha256 7c10fbf888fccd58f5899b3abc87fa021f8a96489b552f8664172c7fcc378044
+ sha256 f527418fb32233f23c7f3c7bfbe5f6830ad56f97acabfcad0ae7c689e673c6b7

:: Comparing captured verification logs...
Done processing logs, no substantive differences found (15561 | 15561 DETLOG messages compared).
:: Failure: nondeterministic.
$ echo $?
1
```

The bytes provably differed between run 1 and run 2 — the stdout comparator
proves it — and the log comparator still reported **15561 of 15561 messages
identical**. That is the blindness, demonstrated on a single pair with no
inference required. It is structural: `recvmsg` returns a stable `1468`, and the
payload bytes never enter the log at all, so there is nothing for the comparator
to compare.

**And the consequence, on the real consumer:**

```
$ ./target/release/hermit run --strict --verify -- python3 -c pass
:: Run1...
:: Run2...
:: Comparing captured verification logs...
Logs contain 92896 | 92896 messages total
Logs contain 75669 | 75669 DETLOG & scheduler COMMIT messages
Normalizing known nondeterministic numerical data before comparison...
  Comparing DETLOG messages...

Done processing logs, no substantive differences found (75669 | 75669 DETLOG messages compared).
:: Success: deterministic. Determinism verified.
$ echo $?
0
```

**Exit 0, "Determinism verified", 75669 of 75669 messages agreeing.** `python3 -c
pass` is the real consumer of these bytes — it issues four `NETLINK_ROUTE` dumps
before user code runs (§3) — and it passes the gate.

One honesty note on this second result: I did not instrument these two
particular runs, so I cannot state that the four bytes differed *in this exact
pair*. What the pair shows is that the gate returns green. That the bytes
generally do differ between consecutive runs is established separately and
strongly — 6 of 6 consecutive runs gave 6 distinct values (§3), and `tstamp` is
a monotone host clock that advances between any two runs. The blindness itself
does not rest on this run at all; it rests on the probe pair above, where the
divergence is proven and the log comparator still saw nothing.

**So the gate depends on the guest's discretion.** glibc's `getifaddrs()` has no
field for IPv6 cacheinfo and discards it, so a real consumer prints nothing and
passes. The bytes reach guest *memory* on every run and never reach guest
*output* — which is precisely why this went unnoticed, and precisely why a test
for it cannot be a `--verify` test on a stock program.

### 4.1 "bitwise_parity" — the word in the brief, located in the code and measured

Your note said `--verify` reports `bitwise_parity` TRUE on this divergence. That
is exactly right, and here is where the term lives and what it does and does not
promise.

`hermit-cli/src/bin/hermit/verify.rs:82-97` defines two log-comparison
strictnesses, and the default is the weaker one:

- **`Stripped`** — plain `hermit run --verify`. `strip_lines = true`. Its own
  docstring says *"Tolerant of limited nondeterminism (numbers, addresses, tmp
  paths, and timestamps are normalized before diffing). **NOT a parity claim.**"*
  The normalisation is `strip_log_entry` (`detcore/src/logdiff.rs:209-249`),
  which rewrites **every bare number** to `<NUM>` and every hex literal to
  `<ADDR>`. The comment there calls it *"terrible overkill"*. Under this mode
  `recvmsg(11, 0x…, 0) = Ok(1468)` becomes `recvmsg(<NUM>, <ADDR>, <NUM>) =
  Ok(<NUM>)` — so this comparator could not detect a change in the reply
  *length*, never mind its content.
- **`Canonical`** (`BitwiseInfoV1`) — `--verify-strict`. `strip_lines = false`,
  addresses canonicalised to first-appearance ordinals, the full INFO trace
  compared, *"every other byte — virtual-time timestamps, raw syscall
  argument/result values, counts, sizes, flags — must match exactly"*. The
  acceptance rule is `ComparisonSpec::is_bitwise_parity()`
  (`verify.rs:318-333`), an eleven-clause predicate.

**The point for the fix: even the strict mode is blind here.** Measured, on the
real consumer, under the strongest policy hermit offers:

```
$ ./target/release/hermit run --strict --verify --verify-strict \
    --verify-json verdict-python.json -- python3 -c pass
$ echo $?
0
$ python3 -m json.tool < verdict-python.json
{
    "verified": true,
    "bitwise_parity": true,
    "verdict": "matched",
    "comparison": {
        "strictness": "canonical",
        "compare_logs": true,
        "log_scope": "info",
        "strip_lines": false,
        "canonicalize_addresses": true,
        "full_trace": true,
        "exact_remainder": true,
        ...
```

`"bitwise_parity": true`, with `strip_lines: false` and `exact_remainder: true`.
This is not the comparator being lossy — under `Canonical` it is not. It is that
**the divergent bytes are not in the log at all.** The log records
`recvmsg(11, 0x…, 0) = Ok(1468)`; the 1468 bytes are copied into guest memory and
never written to any log line. Bitwise parity over that stream is bitwise parity
over a stream that does not contain the data in question.

That is the honest one-line statement of where we stand: **the strongest
determinism claim hermit can currently make about a run does not cover the
contents of a netlink reply.** (The verdict file is kept beside this document as
`verdict-python.json`.)

---

## 5. Two claims in the brief that my measurements changed

I was told to carry a latent second source: *"`IFLA_STATS64` and `IFLA_STATS` on
loopback are all-zero today ONLY because nothing in this container uses
loopback."* The first half is confirmed. **The implication is not.**

Confirmed — in the default `--network=local` reply, those attributes are large
and entirely zero:

```
IFLA_STATS64 at offset 280: 200 bytes, 25 counters, all-zero=True
IFLA_STATS   at offset 484:  96 bytes, 24 counters, all-zero=True
```

That is 296 of 1488 bytes, 20% of the reply, currently zero by circumstance.

**Refuted for the default mode.** I made the guest generate loopback traffic and
re-measured. The counters become non-zero — and stay reproducible:

```
$ for i in 1 2 3 4; do ./target/release/hermit run --strict -- \
    python3 .../lo_traffic_probe.py 40 --hex; done
IFLA_STATS64 rx_packets=40 tx_packets=40 rx_bytes=21600 tx_bytes=21600   (x4, identical)

differing byte positions across the 4 runs: 4 -> [772, 773, 776, 777]
any differing byte inside the stats region 284..584? NONE
```

40 packets x 540 bytes = 21600, exactly. Inside hermit's isolated network
namespace the loopback counters are a **pure function of the guest's own
deterministic traffic**, so they reproduce. The only bytes that still vary are
the same four cacheinfo bytes.

**Confirmed for `--network=host`.** There the counters track host traffic and the
leak is 24x larger:

```
$ for i in 1 2 3; do ./target/release/hermit run --network=host -- python3 .../netlink_probe.py --hex; done
lengths {3060}
differing byte positions across 3 runs: 95 of 3060
(an independent rerun of the same command got 92 of 3060, with the first seven
 contiguous groups byte-for-byte identical to the list below)
contiguous groups (offset, len): [(284,2),(292,2),(300,4),(308,4),(488,2),(492,2),(496,8),
 (1040,2),(1048,4),(1056,2),(1064,3),(1096,2),(1104,2),(1112,4),(1776,3),(1784,3),(1792,4),
 (1800,4),(1980,3),(1984,3),(1988,8),(2540,3),(2548,4),(2556,3),(2564,1),(2580,3),(2588,2),
 (2596,2),(2604,2),(2612,4)]
```

Offsets 284-308 and 488-496 are exactly the `IFLA_STATS64`/`IFLA_STATS` blocks
located above; the 8-byte stride is the `u64` counter layout. So the stats leak
is real, it is measurable, and it is **specific to `--network=host`** — a mode
whose own `--help` text already says it *"compromises isolation and deterministic
reproducibility"*.

| mode | reply | interfaces | bytes varying across runs |
|---|---|---|---|
| `--network=local` (default) | 1488 B | 1 (`lo`) | 4 (0.27%) — cacheinfo only |
| `--network=local` + guest loopback traffic | 1488 B | 1 | 4 (0.27%) — cacheinfo only |
| `--network=host` | 3060 B | 2 (`lo`, `eth0`) | 92-95 (~3%) — cacheinfo + stats |

The second claim I softened: I initially suspected that programs relying on the
kernel's *implicit* autobind (no explicit `bind()`) would bypass
`files.rs:2218`. Both consumers I traced — `python3` startup and `ss -a` — do
call `bind()` explicitly, so this is **not** demonstrated. It remains a code-level
possibility (`handle_bind` is the only hook) but I have no program that triggers
it and I am not claiming it as a defect.

---

## 6. Where we need to get to — the gap list

Ordered by what a fix depends on, so progress against it is countable. Ten
items: seven closable with code (G1, G2, G3, G4, G6, G7, G8), three are
test/coverage debt (G5, G9, G10).

| # | gap | today | done when | evidence it is open |
|---|---|---|---|---|
| **G1** | `NETLINK_ROUTE` reply content is never inspected | no code path exists; the protocol number is not even recognised | a `NETLINK_ROUTE` recvmsg diversion exists, with the same in-place / never-resize / fail-open shape as `sock_diag.rs` | `files.rs:1968` tests one protocol; `io.rs:1325` tests one flag |
| **G2** | `IFLA_INET6_CACHEINFO.tstamp` leaks host uptime | untouched | the field is a function of virtual time, not `/proc/uptime` | measured monotone 63108078 → 63108327 over 6 runs; = host uptime x CLK_TCK |
| **G3** | `IFLA_INET6_CACHEINFO.reachable_time` leaks kernel randomness | untouched | the field is drawn from detcore's seeded PRNG, in-range | 6/6 distinct, all in [15000, 45000] |
| **G4** | any fix must not write `0` to either field | n/a | the substituted values are legal: a duration in [base/2, 3·base/2] and a plausible timestamp | `0` means "no reachability timeout", a state the kernel never produces; unlike an inode, these are values a guest can compute with |
| **G5** | the one normaliser's zeroing has never run on a real kernel reply | only synthetic unit tests; the one cell that reaches the wrapper receives 20-byte `NLMSG_DONE` replies with zero diag messages | a test runs a guest that receives a **populated** dump and asserts the inodes came back zeroed | `ss -s` makes 0 `socket()` calls; the compat `ss` cell's replies are all `Ok(20)` and it asserts the output is empty; nothing runs `ss -a` |
| **G6** | `sock_diag.rs` covers 2 of 4 families | `AF_UNIX`, `AF_NETLINK`; `AF_INET`/`AF_INET6` hit `_ => {}` | `inet_diag_msg.idiag_inode` is zeroed too, and `/proc/net/tcp{,6}`, `/proc/net/udp{,6}` get matching text sanitisers | `sock_diag.rs:176-191`; `procfs.rs` has no `/proc/net/tcp` arm |
| **G7** | the normaliser is reachable from `recvmsg()` only | `recvfrom`, `recvmmsg`, `read`, `readv`, `preadv` all bypass it | every receive path that can carry a dump consults the flag, or there is a written reason why it cannot | `is_sock_diag` has exactly one production caller, `io.rs:1325` |
| **G8** | netlink multicast is entirely unhandled | zero code for `setsockopt(SOL_NETLINK, NETLINK_ADD_MEMBERSHIP)` or `bind()` with `nl_groups != 0`; kernel-initiated notifications arrive asynchronously | either handled, or explicitly refused with a clear error rather than silently admitted | `grep ADD_MEMBERSHIP\|nl_groups\|SOL_NETLINK` over `detcore/src` returns nothing; the only hit is a test asserting `nl_groups == 0` |
| **G9** | no determinism gate covers netlink reply content — **including the strict one** | `--verify-strict` reports `"bitwise_parity": true` on `python3 -c pass` with `strip_lines: false, exact_remainder: true`, because the payload bytes are never logged | netlink reply bytes are covered by *some* gate — e.g. a content digest emitted into the DETLOG at `recvmsg` on netlink fds, or a dedicated test whose guest prints the bytes | measured, §4 and §4.1; verdict JSON kept beside this document |
| **G10** | the C-guest autobind fixtures are outside blocking CI | `tests/c/netlink_autobind_{route,usersock,generic}.c` are `modes.verify.ci: false` (`tests/e2e/manifests/c-programs.yaml`) | either they run in blocking CI, or the manifest says why they are redundant with the Rust test | inventory `why` field: *"remains outside blocking CI until its standalone build and output contract are calibrated"*. **Lower priority than I first thought** — the Rust test at `misc/mod.rs:929` does cover the same ground cross-run in blocking CI, so this is duplication debt, not a coverage hole |

**Sequencing.** G2/G3 are the concrete defect and depend on G1 (there is nowhere
to put the fix until a `NETLINK_ROUTE` path exists). G4 is a constraint on G1-G3,
not separate work. G5 and G9 are the reason this was invisible and should land
alongside G1 rather than after it — otherwise a `NETLINK_ROUTE` normaliser
inherits `sock_diag.rs`'s situation of being unproven on real bytes. G6, G7, G8,
G10 are independent and can be taken in any order.

**Explicitly out of scope:** `--network=host` (G-none). Its `--help` text already
documents it as non-deterministic by construction, and the ~90-byte measurement in
§5 is consistent with that. It sizes the risk; it is not a bug to fix here.

---

## Appendix: how to reproduce anything above

All from `/home/newton/work/dev-hermit/hermit` at `fda2b0b2874f`, with
`./target/release/hermit`.

```bash
# The syscall trace for any guest (DETLOG lines require --log=info or finer):
./target/release/hermit --log=info run -- <program> 2>&1 >/dev/null | grep "inbound syscall"

# Which netlink protocols a program opens (16 = AF_NETLINK; 3rd arg = protocol):
... | grep -oE "socket\(16, [0-9]+, [0-9]+\)" | sort | uniq -c

# Capture a raw RTM_GETLINK dump (probe lives in the sibling experiment dir):
./target/release/hermit run --strict -- \
  python3 ../experiments/netlink-leak_20260820/netlink_probe.py --hex | tail -1

# Loopback-traffic variant used in section 5:
./target/release/hermit run --strict -- \
  python3 ../experiments/netlink-start-and-target_20260820/lo_traffic_probe.py 40 --hex

# The --verify blindness, both halves:
./target/release/hermit run --strict --verify -- python3 -c pass                    # exit 0
./target/release/hermit run --strict --verify -- python3 .../netlink_probe.py       # exit 1

# The same under the STRICT parity policy, with the machine-readable verdict:
./target/release/hermit run --strict --verify --verify-strict \
  --verify-json verdict.json -- python3 -c pass   # exit 0, "bitwise_parity": true
```

**Two traps worth knowing, both hit during this work:**

1. A probe script placed under `/tmp` is invisible to the guest — hermit's
   namespace replaces `/tmp`. `python3: can't open file '/tmp/probe.py'`. Keep
   probes under the repo tree.
2. `--verify-strict` is much more expensive than `--verify`. `python3 -c pass`
   completed, but the netlink probe did not finish inside a 420 s `timeout` and
   was killed. The resulting verdict file is not empty and not obviously
   broken — it reads `{"verified": false, "bitwise_parity": false, "verdict":
   "no_result", "comparison": null}`. **`verdict: "no_result"` with
   `comparison: null` means the comparison never ran**, not that the run
   diverged. Anything consuming these verdicts in the test work must
   distinguish those two, or a timeout will be scored as a determinism failure.
