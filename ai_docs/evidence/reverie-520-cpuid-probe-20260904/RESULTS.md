# Results

Numbers as observed. `RERUN.md` says how to get them again; `logs/` holds the raw
output each line was read from.

## 1. The defect, measured

With the base CPUID table, every leaf `0xd` subleaf the guest asks for returns
subleaf 0's row — subleaves 0 through 20 and 63 all read
`00000007 00000340 00000340 00000000`. EAX `0x7` at subleaf 1 sets bits 0, 1 and 2,
so the guest is told XSAVEOPT, XSAVEC and XGETBV_ECX1 are all supported.

With either PR head, only subleaves 0 and 2 carry data:

```
0xd.0  00000007 00000340 00000340 00000000
0xd.1  00000000 00000000 00000000 00000000
0xd.2  00000100 00000240 00000000 00000000
0xd.3 .. 0xd.20, 0xd.63   all zero
```

Logs: `host-glibc234-dump-base-table.log`,
`host-glibc234-dump-head-3bf292b2.log`, `host-glibc234-dump-head-4fcf9df5.log`.

Side effect beyond leaf `0xd`: out-of-range leaves change too. `CPUID.0x15` with
ECX=5 returned the aliased `0xd` row before and returns zeros after, because KVM
redirects an out-of-range basic leaf to the max basic leaf and then applies the
subleaf match.

## 2. The A/B, reverie only, no Hermit

Changing nothing but `reverie-kvm/src/cpuid.rs`:

| | host glibc 2.34 | image glibc 2.42 |
|---|---|---|
| base table, lazily bound `puts` | exit 0 | **#GP, vector 13, RIP `0x1014dac`** |
| base table, eagerly bound (`-z now`) | exit 0 | exit 0 |
| PR head, either binding | exit 0 | exit 0 |

`0x1014dac` is 16862636 decimal, the same faulting address PR 520 reports from its
Hermit run. Two things follow that the pull request does not state:

- The defect is glibc-version-dependent. On glibc 2.34 a guest containing only
  `puts("...")` and no CPUID reads exits 0 even on the broken table.
- The fault is in the lazy PLT resolution trampoline, not in loader startup —
  eager binding survives the broken table.

Logs: `image-glibc242-ab-3bf292b2.log`,
`image-glibc242-eager-vs-lazy-3bf292b2.log`, `image-glibc242-ab-4fcf9df5.log`.

## 3. Leaf 0xd subleaf 0 EBX is not supplied by the table

Sentinel run: table subleaf 0 EBX set to `0xdeadbeef`, table subleaf 2 EAX set to
`0x200`.

```
0xd.0 00000007 00000340 00000340 00000000   <- table said 0xdeadbeef
0xd.2 00000200 00000240 00000000 00000000   <- table said 0x200, and it arrived
```

`0x340` is the host's `CPUID.0xd.2` EBX (`0x240`) plus EAX (`0x100`), both measured
on this host — so KVM recomputes that register from the host's CPUID. The same
thing from the other direction: at `4fcf9df5` the table supplies `0x0` for that
field and the guest still reads `0x340`, and the guest-visible dump is
byte-identical to `3bf292b2`'s.

A guard now holds this in place. The real-mode assertion in
`reverie-kvm/tests/vmcall.rs` requires EBX `0x240`; if KVM stopped overwriting the
field the guest would read `0x0` and that assertion would fail. **The zero in the
table is safe only because that assertion holds it there.**

Log: `host-glibc234-sentinel-subleaf0-ebx.log`.

## 4. Which BIND_NOW marker exists on an eagerly bound binary

```
lazy: DT_BIND_NOW=false  flags=0x0 (DF_BIND_NOW set: false)  flags_1=0x0 (DF_1_NOW set: false)
 now: DT_BIND_NOW=true   flags=0x0 (DF_BIND_NOW set: false)  flags_1=0x1 (DF_1_NOW set: true)
```

On this toolchain an eagerly bound binary carries no `DT_FLAGS` entry at all, so
`info.flags` is zero. **A check written only against `DF_BIND_NOW` passes an
eagerly bound guest** — it would have looked correct and caught nothing. The
merged test asserts all three markers, so two of them fire.

End to end: flipping the merged test's own compile flag to `-Wl,-z,now` fails with
`test guest must not contain DT_BIND_NOW`.

Logs: `bind-now-marker-fields.log`, `bind-now-assertion-power.log`.

## 5. Which rows and guards are load-bearing

- Deleting the subleaf 1, 17, 18 and 19 rows from `DETERMINISTIC_XSTATE_CPUIDS`
  leaves every test green — 5 lib, 1 vmcall, 1 dynamic guest. Those four rows are
  all-zero and KVM's miss path already returns zeros, so they are
  indistinguishable from absence. Only subleaves 0 and 2 carry information.
  Subleaves 17, 18 and 19 are glibc's probe ceiling — `X86_XSTATE_TILECFG_ID`,
  `TILEDATA_ID` and the `APX_F_ID` added in glibc 2.41, which is
  `X86_XSTATE_MAX_ID` in 2.42 — not a specification boundary.
- Deleting the `assert_unique_function_indices(&entries);` call from
  `deterministic_cpuid_table()` leaves all 5 lib tests green, so the runtime
  refusal's wiring is untested. A duplicate would still be caught, because one
  lib test calls the helper directly on the real table.
- Restoring the old leaf `0xd` row at `DETERMINISTIC_STANDARD_CPUIDS[13]` fails 3
  of 5 lib tests with `fixed CPUID profile contains duplicate leaf 0xd, subleaf
  0x0`. The guard has real power against the regression it exists for.

Logs: `inert-rows-deleted-*.log`, `guard-callsite-deleted.log`,
`duplicate-row-restored.log`.

## 6. Do the merged regression tests have power?

With only `cpuid.rs` reverted to base, at both heads:

- `deterministic_cpuid_policy_is_visible_inside_vm` fails — leaf `0xd` subleaf 1
  reads `[7, 576, 832, 0]` where `[0, 0, 0, 0]` is expected.
- `dynamic_c_guest_observes_indexed_xstate_cpuid_and_lazy_binding` fails with
  guest exit 11, the subleaf 1 case, at both heads. `4fcf9df5` renumbered the
  guest's exit codes from `10 + i` to `11 + i` after hoisting subleaf 0 out of
  the array; exit 11 at both heads confirms the mapping was preserved rather than
  shifted.

Logs: `base-revert-*`.

## 7. Package state

At both heads: 254 passed, 0 failed, zero skip lines under
`REVERIE_REQUIRE_KVM=1 cargo test -p reverie-kvm -- --test-threads=1`, matching
the count PR 520 states. `cargo clippy -p reverie-kvm --all-targets -- -D warnings`
and `cargo fmt -p reverie-kvm --check` both clean at both heads.

Logs: `full-suite-3bf292b2.log`, `full-suite-4fcf9df5.log`.

## Findings left open at `4fcf9df5`

The two blocking findings were fixed and the changes-requested marker withdrawn.
Three non-blocking findings stand:

- The four inert all-zero subleaf rows above.
- No test expects the panic from `deterministic_cpuid_table()` itself.
- Outside this pull request: `hermit/detcore/src/lib.rs` receives `ecx` in
  `handle_cpuid_event` and discards it, the same shape PR 520 fixes on the KVM
  side. The KVM backend never reaches that path, so the two backends show the
  guest different CPUID — leaf 1 ECX bits 26 and 27, leaf 7 EBX, and leaf `0xd`
  all differ.
