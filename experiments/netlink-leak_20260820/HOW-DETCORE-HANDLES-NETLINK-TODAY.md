# How detcore handles netlink today

Agent: detdebug (claude) | Task: `write-up-how-detcore-handles-netlink-today` | Date: 2026-08-20
Repo: `hermit/` at the checkout in `/home/newton/work/dev-hermit/hermit`
Every claim below carries a file:line or a runnable command with its real output.

---

## 0. First: two corrections, because I caused both misreadings

### 0.1 No zero has been introduced. There is no regression.

The question asked was: *"was this a bug, when did we introduce a zero return
value?"*

**Nothing in detcore returns zero for the field in question, and nothing ever
did.** There is no commit to find and no regression to date.

What I actually wrote was a warning about a *fix that has not been written yet*.
The leaking field is a kernel timer interval. If someone "determinizes" it the
obvious way — by overwriting it with `0` — that is not a neutral constant, it is
an **illegal value** for that field: it means "no reachability timeout", which
is a state the kernel never produces. A guest that reads it can legitimately
behave differently than it would on any real machine. So the warning was
*prospective*: **do not fix this by zeroing.** I stated it next to the defect,
and it read as though the zero already existed. It does not.

### 0.2 `detcore/src/sock_diag.rs` is not a `NETLINK_ROUTE` normaliser

The brief for this write-up says "the existing `NETLINK_ROUTE` normaliser" in
`sock_diag.rs`. That is wrong, and the distinction is the whole point of the
document.

`sock_diag.rs` normalises **`NETLINK_SOCK_DIAG`** — see its own module docstring,
`detcore/src/sock_diag.rs:8`:

> `//! Determinize socket inode numbers in NETLINK_SOCK_DIAG dump replies.`

`NETLINK_SOCK_DIAG` and `NETLINK_ROUTE` are **two different protocols carried
over the same socket type**, selected by a number at socket-creation time
(`NETLINK_ROUTE` = 0, `NETLINK_SOCK_DIAG` = 4). The leak I reported is on
`NETLINK_ROUTE`. `sock_diag.rs` never sees those bytes — the code that routes
into it tests the protocol explicitly, at `detcore/src/syscalls/files.rs:1968`:

```rust
if call.family() == libc::AF_NETLINK && call.protocol() == libc::NETLINK_SOCK_DIAG {
```

So `sock_diag.rs` is a **precedent** — it is the shape a `NETLINK_ROUTE`
normaliser would take — but it is **not** the code that would need changing, and
it is not currently doing anything about the leak.

---

## 1. Vocabulary

Skip if you know it.

- **netlink** — a socket-based way for a program to ask the Linux kernel
  questions. You `socket(AF_NETLINK, ...)`, `sendmsg()` a request, `recvmsg()` a
  reply. It is how `ip`, `ss`, and glibc's `getifaddrs()` work. It is *not* a
  network protocol; nothing leaves the machine.
- **protocol number** — the third argument to `socket()`. It picks *which kernel
  subsystem* answers. `NETLINK_ROUTE` (0) answers questions about network
  interfaces, addresses, and routes. `NETLINK_SOCK_DIAG` (4) answers questions
  about the socket table (this is what `ss` uses).
- **`RTM_GETLINK`** — the `NETLINK_ROUTE` request meaning "describe the network
  interfaces". This is the request in the leak.
- **attribute (`rtattr`)** — netlink replies are not fixed structs. After a small
  header comes a chain of type-length-value records. Each has a 2-byte length,
  a 2-byte type code, then payload, padded to 4 bytes. Attributes nest.
- **`IFLA_*`** — the type codes used inside an `RTM_GETLINK` reply. The ones that
  matter here, with the enum lines that define them in
  `/usr/src/kernels/6.19.2-0_fbk0_hardened_rc25_0_g1d99d2f6053c/include/uapi/linux/if_link.h`:

  | code | value | declared at | what it holds |
  |---|---|---|---|
  | `IFLA_STATS` | 7 | if_link.h:304 | 32-bit packet/byte counters for the interface |
  | `IFLA_STATS64` | 23 | if_link.h:329 | the same counters, 64-bit — the modern one |
  | `IFLA_AF_SPEC` | 26 | if_link.h:332 | a *container*: per-address-family sub-attributes nested inside |
  | `IFLA_INET6_CACHEINFO` | 5 | if_link.h:449 | (inside `IFLA_AF_SPEC` → `AF_INET6`) IPv6 neighbour-cache timers |

  The values are small and `IFLA_INET6_CACHEINFO`'s collides numerically with
  the others because it belongs to a **different enum** (`IFLA_INET6_*`), which
  is only meaningful once you are already inside the `AF_INET6` nest. Attribute
  type codes are namespaced by their container; the same number means different
  things at different depths. Values resolved by compiling against the header,
  not read off the line numbers:

  ```
  $ printf '#include <linux/if_link.h>\n#include <stdio.h>\nint main(){printf("IFLA_STATS=%d IFLA_STATS64=%d IFLA_AF_SPEC=%d IFLA_INET6_CACHEINFO=%d\n",IFLA_STATS,IFLA_STATS64,IFLA_AF_SPEC,IFLA_INET6_CACHEINFO);}\n' > /tmp/ifl.c && gcc -o /tmp/ifl /tmp/ifl.c && /tmp/ifl
  IFLA_STATS=7 IFLA_STATS64=23 IFLA_AF_SPEC=26 IFLA_INET6_CACHEINFO=5
  ```

  `IFLA_STATS64` is worth naming explicitly because it is the *obvious* suspect
  for nondeterminism — traffic counters — and it is **not** where this leak is.
  On an isolated loopback interface those counters are reproducible.

- **`struct ifla_cacheinfo`** — the payload of `IFLA_INET6_CACHEINFO`. Four
  `__u32`s, at `if_link.h:1105-1110`:

  ```c
  struct ifla_cacheinfo {
      __u32 max_reasm_len;
      __u32 tstamp;         /* ipv6InterfaceTable updated timestamp */
      __u32 reachable_time;
      __u32 retrans_time;
  };
  ```

  `reachable_time` is the leaking field: bytes 8..12 of that payload.

---

## 2. What detcore does with netlink today

Three things. Two are active interventions; the third is the gap.

### 2.1 It determinizes the socket's own identity on `bind()`

When a program binds a netlink socket with `nl_pid == 0` it is asking the kernel
to pick an identifier ("autobind"). The kernel normally picks the thread's PID,
or a unique high value if that is taken — either way it is host state.

detcore substitutes a deterministic one. `detcore/src/syscalls/files.rs:2218-2231`:

```rust
} else if sockaddr_family == libc::AF_NETLINK as u16
    && call.addrlen() >= std::mem::size_of::<libc::sockaddr_nl>() as i32
{
    let mut sockaddr_nl: libc::sockaddr_nl = guest
        .memory()
        .read_value(addr.cast::<libc::sockaddr_nl>())?;
    if sockaddr_nl.nl_pid == 0 {
        let resp = send_and_update_time(guest, GlobalRequest::RequestPort(open_file_id)).await;
        match resp.1 {
            GlobalResponse::RequestPort(port) => {
                sockaddr_nl.nl_pid = DETERMINISTIC_NETLINK_PORT_ID_BASE | u32::from(port);
```

The base constant is at `files.rs:107`, with its rationale at `:106`:

```rust
// Above Linux's PID range and below the high-bit IDs used by kernel autobind.
const DETERMINISTIC_NETLINK_PORT_ID_BASE: u32 = 0x4000_0000;
```

This applies to **all** netlink protocols including `NETLINK_ROUTE`. It
determinizes *who the socket is*. It does nothing to *what the kernel says*.

Provenance: marked `AUTONOMOUS-BOT-IMPLEMENTED` / `TODO-HUMAN-REVIEW(PR-880)` at
`files.rs:2215-2216`.

### 2.2 It rewrites `NETLINK_SOCK_DIAG` reply payloads

Two-step. First, sockets of that protocol are flagged at creation —
`files.rs:1967-1973`:

```rust
fn mark_sock_diag_fd<G: Guest<Self>>(&self, guest: &mut G, fd: RawFd, call: &syscalls::Socket) {
    if call.family() == libc::AF_NETLINK && call.protocol() == libc::NETLINK_SOCK_DIAG {
        let _ = guest.thread_state().with_detfd(fd, |detfd| detfd.set_sock_diag());
    }
}
```

The flag lives on the file description (`detcore/src/fd.rs:131`, setter at
`:437`, getter at `:444`), so it survives `dup()`.

Second, `recvmsg` on a flagged descriptor diverts into the sanitizing path.
**This is in `io.rs`, not `files.rs`** — `detcore/src/syscalls/io.rs:1319-1329`,
at the very top of `handle_recvmsg` (which begins at `io.rs:1314`):

```rust
// NETLINK_SOCK_DIAG replies carry host-assigned socket inode numbers in
// their msg_iov payload (not msg_control). Determinize them so the
// binary socket-diag path agrees with the /proc/net/* text sanitizers.
if self.cfg.virtualize_metadata
    && guest
        .thread_state()
        .with_detfd(call.sockfd(), |detfd| detfd.is_sock_diag())
        .unwrap_or(false)
{
    return self.handle_sock_diag_recvmsg(guest, call).await;
}
```

That handler gathers the scattered `iovec` segments into one buffer, calls the
parser, and writes the bytes back preserving the original segment boundaries —
`io.rs:1436-1446`.

I checked whether this was wired at all, because a written-and-tested-but-never-
called normaliser is a pattern this project has hit before. **It is genuinely
wired**: `io.rs:1436` is a production call site, not a test.

```
$ grep -rn "sanitize_sock_diag_inodes" --include='*.rs' hermit/ | grep -v scratch/ | grep -v /target/
detcore/src/sock_diag.rs:60         (definition)
detcore/src/sock_diag.rs:304,344,345,358,365,380,390,400   (its own tests)
detcore/src/syscalls/io.rs:1436     (production)
```

### 2.3 It does nothing at all to `NETLINK_ROUTE` reply payloads

There is no equivalent of §2.2 for `NETLINK_ROUTE`. The `recvmsg` diversion at
`io.rs:1322` tests `is_sock_diag()`, which is only ever set for protocol 4
(`files.rs:1968`). An `RTM_GETLINK` reply is copied into the guest byte-for-byte
as the kernel produced it.

That is the leak: the host kernel's per-boot random `reachable_time` reaches the
guest untouched.

---

## 3. `detcore/src/sock_diag.rs` — what, why, when

**What.** One public function, `sanitize_sock_diag_inodes(buf: &mut [u8]) -> bool`
(`sock_diag.rs:60`). It walks the multipart netlink framing of a socket-dump
reply and **zeroes the socket inode numbers** in place: `udiag_ino` for `AF_UNIX`
bodies (offset 4, `sock_diag.rs:45`), `ndiag_ino` for `AF_NETLINK` bodies
(offset 16, `sock_diag.rs:52`), and the peer inode carried in the
`UNIX_DIAG_PEER` attribute (`sock_diag.rs:49`).

**Why zero is legal *there* and not on our field.** An inode number is an opaque
identity token; the guest uses it only to correlate rows against
`/proc/net/unix` and `/proc/net/netlink`, and detcore already zeroes it in those
*text* files, so zeroing the binary path makes the two agree. The docstring says
exactly this at `sock_diag.rs:19-22`:

> `//! detcore already zeroes the same inodes in the procfs *text* interfaces`
> `//! (/proc/net/unix, /proc/net/netlink; see crate::procfs). This module`
> `//! applies the identical zeroing to the *binary* SOCK_DIAG path so both paths`
> `//! agree.`

`reachable_time` is not an identity token. It is a **duration in milliseconds**
that the guest can compute with. Zero is out of range, not anonymous. This is the
distinction §0.1 was about.

**Design properties worth copying.** `sock_diag.rs:24-28`:

> `//! The parser is deliberately pure and fail-open: on any framing`
> `//! inconsistency it leaves the buffer untouched ... It only ever *zeroes*`
> `//! fixed-width fields in place; it never grows, shrinks, or shifts the`
> `//! buffer, so a partial parse can never corrupt message boundaries.`

A `NETLINK_ROUTE` normaliser should have the same in-place, never-resize,
fail-open shape — the value substituted is the only thing that should differ.

**When.** Single commit, `f5e16c3303` — *"detcore: determinize NETLINK_SOCK_DIAG
dump inodes and message order"*. It is on `origin/main`.

```
$ git log --oneline --diff-filter=A origin/main -- detcore/src/sock_diag.rs
f5e16c3303 detcore: determinize NETLINK_SOCK_DIAG dump inodes and message order
```

Caveat on dating it: in *this* checkout the file shows as staged-added
(`git status --porcelain` → `A  detcore/src/sock_diag.rs`) and `git log` on the
working branch returns zero commits for it, because this tree is mid-rebuild
against a different base. The `origin/main` history above is the authoritative
one. It is marked `AUTONOMOUS-BOT-IMPLEMENTED` / `TODO-HUMAN-REVIEW(PR-1064)`
(`sock_diag.rs:30-31`) — i.e. it has not had human review.

---

## 4. Which flags affect this path, and what each actually does

Only one flag gates the existing netlink sanitizing, and it is not a networking
flag.

- **`--virtualize-metadata` / `--no-virtualize-metadata`** — this is the *only*
  gate on the sock_diag sanitizer, at `io.rs:1322` (`self.cfg.virtualize_metadata`).
  With `--no-virtualize-metadata` the diversion is skipped and raw host inode
  numbers reach the guest. It has no effect on the `NETLINK_ROUTE` leak, because
  nothing is gated there in the first place.

- **`--network <local|host>`**, default `local`. From `hermit run --help`:

  > `Select guest networking. `local` creates an isolated loopback interface;`
  > `host` exposes the host network and compromises isolation and deterministic`
  > `reproducibility`

  This one materially changes the *size* of the problem. Under the default
  `local` the guest sees one interface (`lo`), which is why the leak I measured
  is a single 4-byte field in a 1468-byte reply. Under `--network=host` the
  reply enumerates every host interface, each with its own cacheinfo timers and
  live `IFLA_STATS64` counters. **The help text already warns that `host`
  compromises reproducibility**, so that mode is out of scope for a fix — it is
  documented as non-deterministic by construction.

- **`--analyze-networking`** — from `--help`: *"Diagnose non-zero network binds.
  Implies an isolated network namespace and conflicts with `--network=host`."*
  A diagnostic, not a determinism control.

- **`--no-deterministic-io`** (and the other `--no-*` switches) are bundled by
  `--strict`'s inverse; per `--help`, the sequence-of-shorthand flag documents
  itself as *"This does not determinize execution."* Not relevant to this path
  beyond the metadata flag above.

Note also `DETERMINISTIC_NETNS_COOKIE: u64 = 1` at `files.rs:104`, whose comment
states the operating assumption the `local` mode rests on:

```rust
// Hermit exposes exactly one isolated guest network namespace.
```

---

## 5. Why `reachable_time` varies — confirmed, with the limit of the confirmation

The value is set by the kernel through `neigh_set_reach_time()`, at
`/usr/src/kernels/6.19.2-.../include/net/neighbour.h:383-388`:

```c
static inline void neigh_set_reach_time(struct neigh_parms *p, unsigned long base)
{
    WRITE_ONCE(p->reachable_time, neigh_rand_reach_time(base));
}
```

with `neigh_rand_reach_time(unsigned long base)` **declared** at
`neighbour.h:381`. The name says randomised, and RFC 4861 §6.3.2 requires
exactly this — hosts must randomise the reachable time within a factor of the
configured base so that neighbour probes from many hosts do not synchronise.

**What I could not do:** read the function body. The installed kernel tree here
is headers-only — `neigh_rand_reach_time`'s implementation lives in
`net/core/neighbour.c`, which is not present. So I did **not** confirm the exact
distribution by reading source, and I am not going to assert the formula.

**What I did instead — empirical confirmation.** The base is a sysctl:

```
$ cat /proc/sys/net/ipv6/neigh/lo/base_reachable_time_ms
30000
```

If the randomisation is the RFC-standard uniform draw over [base/2, 3·base/2],
the legal range is **[15000, 45000]** ms. That is a prediction, so I made it
before looking at the samples. Twelve values of `lo`'s `reachable_time`
harvested from `RTM_GETLINK` replies under hermit:

```
16228 25273 25798 29247 30041 31021 32246 35555 38228 39643 42127 42607
```

- **12 of 12 inside [15000, 45000]** — min 16228, max 42607.
- **12 distinct values**, spread across the interval rather than clustered.

That is consistent with a uniform draw over the predicted range and inconsistent
with any fixed or slowly-varying value. Combined with the declaration name and
the RFC requirement, I am confident the variation is deliberate kernel
randomisation. I am *not* claiming the precise generator.

**Scope of what varies.** Of the four `struct ifla_cacheinfo` fields, the one I
measured differing between runs is `reachable_time`. I have not established
run-to-run behaviour for `tstamp` (a jiffies-domain timestamp, which would be
expected to vary if the interface is created at different host uptimes) — that
is an open item, not a claim in either direction.

---

## 6. Summary for the decision

| | today |
|---|---|
| netlink socket identity (`nl_pid`) | determinized — `files.rs:2218-2231` |
| `NETLINK_SOCK_DIAG` reply inodes | zeroed — `io.rs:1436` → `sock_diag.rs:60`, gated on `virtualize_metadata` |
| `NETLINK_ROUTE` reply payload | **untouched** — no code path exists |
| the leaking field | `IFLA_AF_SPEC` → `AF_INET6` → `IFLA_INET6_CACHEINFO` → `reachable_time`, 4 bytes |
| why it varies | kernel randomises it per RFC 4861 §6.3.2; confirmed empirically, 12/12 in the predicted range |
| what a fix must not do | write `0` — out of range for a duration field, unlike the inode case that precedent is drawn from |

---

## 7. Addendum (same day): whose syscalls are these?

Asked after delivery, because the answer changes what a fix means. **These are
host-userspace syscalls made by a process running under hermit's interception,
serviced by the host kernel.** They are *not* a guest Linux kernel servicing
anything, and no VM is involved.

Evidence — one command, its real output:

```
$ ./target/release/hermit --log=info run -- python3 -c pass 2>&1 \
    | grep -E "dtid 3\] .*(socket\(16|recvmsg\(11)" | head -4
DETLOG [syscall][detcore, dtid 3] inbound syscall: socket(16, 524291, 0) = ?
DETLOG [syscall][detcore, dtid 3] finish syscall #840: socket(16, 524291, 0) = Ok(11)
DETLOG [syscall][detcore, dtid 3] inbound syscall: recvmsg(11, 0x7fffffff86b0, 0) = ?
DETLOG [syscall][detcore, dtid 3] finish syscall #845: recvmsg(11, 0x7fffffff86b0, 0) = Ok(1468)
```

- `socket(16, 524291, 0)` decodes as `AF_NETLINK`, `SOCK_RAW|SOCK_CLOEXEC|SOCK_NONBLOCK`,
  protocol `0` = **`NETLINK_ROUTE`**. That is glibc's `getifaddrs()` opening its
  netlink socket.
- The line is a `DETLOG [syscall]` record, which by construction means **hermit
  intercepted this syscall**. Only host syscalls from processes inside hermit's
  container produce these.
- The only program in the container is python3 — the single `execve` in the
  whole run is `/usr/local/fbcode/platform010/bin/python3.12`.
- `pgrep -c qemu-system-x86_64` → `0`. No VM was running.

### The word "guest" is overloaded, and that is what caused the confusion

- **hermit's "guest"** = the userspace process hermit instruments. Its syscalls
  go to the **host** kernel; hermit intercepts and sanitizes the results. There
  is no emulated kernel anywhere in a plain `hermit run`.
- **QEMU's "guest"** = the emulated machine and the real Linux kernel running
  inside it.

When QEMU runs under hermit, QEMU is *hermit's* guest. So:

| | intercepted by hermit? | affected by a NETLINK_ROUTE normaliser? |
|---|---|---|
| python3 / QEMU calling `getifaddrs()` on the host | yes | **yes** |
| the Linux kernel inside a QEMU VM doing its own netlink | **no** | no |

The emulated kernel's netlink traffic never becomes a host syscall — it is
serviced inside QEMU's emulated memory, so hermit cannot see it and cannot
change it.

### What this means for the decision

The framing "normalising this changes what the guest kernel observes, and the
guest kernel is the system under test" **does not apply to this leak**. The fix
would change what a hermit-instrumented *host* process sees about *host* network
interfaces — a much less delicate tradeoff. The affected consumer is code like
glibc's `getifaddrs()` in python3, not an OS under test.

The §0.1 warning still stands unchanged: whatever value is substituted must be
in-range for a duration field. `0` is not.

Not verified, and not needed for this answer: `dtid 3` versus `dtid 7` — the run
logs exactly one `execve` (python3) but issues the netlink calls under a
different dtid. Both are processes inside hermit's container either way, which
is the only property this conclusion rests on.
