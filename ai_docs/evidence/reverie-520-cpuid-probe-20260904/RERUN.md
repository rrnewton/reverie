# How to rerun

Every measurement below was run from a worktree of `rrnewton/reverie`. Drop
`review520_probe.rs` into `reverie-kvm/tests/` first; it needs nothing else.

Requirements: a readable-writable `/dev/kvm`, `/usr/bin/gcc`, and — for the
glibc 2.42 legs — podman and the image named in `PROVENANCE.md`.

## The one trap that will silently give you the wrong answer

**The A/B leg is "revert only `reverie-kvm/src/cpuid.rs` to the base SHA", not
"check out the base SHA."** At the base the PR's tests do not exist, so checking
it out measures nothing.

```bash
git checkout ce841d744cc74b1627ac52b42f711b33b1c72a45 -- reverie-kvm/src/cpuid.rs
# ... measure ...
git checkout <the head you are reviewing> -- reverie-kvm/src/cpuid.rs
```

**And copy each built binary out of `target/debug/deps/` before rebuilding the
other leg.** Cargo reuses the same filename, so building the second leg
overwrites the first and you end up running one table twice. This happened once
during the review and produced a dump that looked like the head's while carrying
a sentinel value; it was caught only because the sentinel was visible in the
output.

```bash
cargo test -p reverie-kvm --test review520_probe --no-run
cp "$(ls target/debug/deps/review520_probe-* | grep -v '\.d$' | head -1)" /tmp/probe-head
```

## 1. Guest-visible CPUID, on the host's libc

```bash
cargo test -p reverie-kvm --test review520_probe probe_dump_all_xstate_subleaves \
  -- --exact --nocapture
```

Dumps leaf `0xd` subleaves 0-20 and 63, leaf 1, and leaf `0x15` at ECX 0 and 5,
read from inside the VM. Run it once per leg.
Logs: `host-glibc234-dump-base-table.log`, `host-glibc234-dump-head-3bf292b2.log`,
`host-glibc234-dump-head-4fcf9df5.log`.

## 2. Is leaf 0xd subleaf 0 EBX supplied by the table?

Edit `DETERMINISTIC_XSTATE_CPUIDS` in `reverie-kvm/src/cpuid.rs` to a sentinel —
subleaf 0 EBX to `0xdead_beef` and subleaf 2 EAX to `0x0000_0200` — rebuild, and
run the dump above. Subleaf 2's sentinel reaches the guest; subleaf 0's EBX does
not. Log: `host-glibc234-sentinel-subleaf0-ebx.log`.

## 3. The glibc 2.42 A/B, with no Hermit

Build both legs, then run both binaries inside the image. The harness runs on the
host's libc through the host loader; the guest C program is compiled by the
image's gcc and so links against glibc 2.42, which is the part that matters. The
image has `/bin/gcc` but not `/usr/bin/gcc`, which the tests hardcode.

```bash
IMG=localhost/hermit-hermetic-validate@sha256:e38c3b2d5cd8a17ed2f99b4a24dd76c9ee63329cdc8c9723e4650ab085fae985
podman run --rm --device /dev/kvm \
  -v /tmp/probe-head:/probe-head:ro -v /tmp/probe-base:/probe-base:ro \
  -v /usr/lib64:/hostlib64:ro "$IMG" /bin/sh -c '
    mkdir -p /usr/bin && ln -sf /bin/gcc /usr/bin/gcc
    for p in /probe-head /probe-base; do
      echo "===== $p ====="
      /hostlib64/ld-linux-x86-64.so.2 --library-path /hostlib64 "$p" \
        --nocapture --test-threads=1
    done'
```

Logs: `image-glibc242-ab-3bf292b2.log`, `image-glibc242-ab-4fcf9df5.log`,
`image-glibc242-eager-vs-lazy-3bf292b2.log`.

## 4. Which BIND_NOW marker fields actually exist

```bash
cargo test -p reverie-kvm --test review520_probe \
  probe_bind_now_markers_on_an_eager_binary -- --exact --nocapture
```

Log: `bind-now-marker-fields.log`.

## 5. Does the merged lazy-binding assertion have power?

Change the compile flag in
`dynamic_c_guest_observes_indexed_xstate_cpuid_and_lazy_binding` from
`-Wl,-z,lazy` to `-Wl,-z,now` and run that test. It must fail.

```bash
REVERIE_REQUIRE_KVM=1 cargo test -p reverie-kvm --test static_elf \
  dynamic_c_guest_observes_indexed_xstate_cpuid_and_lazy_binding -- --exact
```

Log: `bind-now-assertion-power.log`.

## 6. Do the PR's own regression tests have power?

With only `cpuid.rs` reverted to base, both must fail:

```bash
REVERIE_REQUIRE_KVM=1 cargo test -p reverie-kvm --test vmcall \
  deterministic_cpuid_policy_is_visible_inside_vm -- --exact
REVERIE_REQUIRE_KVM=1 cargo test -p reverie-kvm --test static_elf \
  dynamic_c_guest_observes_indexed_xstate_cpuid_and_lazy_binding -- --exact
```

Logs: `base-revert-*`.

## 7. Which table rows and guards matter

- Delete `assert_unique_function_indices(&entries);` from
  `deterministic_cpuid_table()` → `guard-callsite-deleted.log`.
- Restore the old leaf `0xd` row at `DETERMINISTIC_STANDARD_CPUIDS[13]` →
  `duplicate-row-restored.log`.
- Delete the subleaf 1, 17, 18 and 19 rows from `DETERMINISTIC_XSTATE_CPUIDS` →
  `inert-rows-deleted-*.log`.

Move `review520_probe.rs` out of `reverie-kvm/tests/` before running the full
package, or its tests are counted alongside the 254.
