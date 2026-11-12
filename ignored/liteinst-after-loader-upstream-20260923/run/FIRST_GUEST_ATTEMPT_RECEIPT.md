# First reviewed real-guest attempt

The candidate bound by `STAGING_RECEIPT.md` SHA-256
`5d9e33b9e006cf6f9e1c7c109e9f44418f858b86d08254577cf820a30fd7ba2f`
was authorized for six exact, separate real-guest cells. Each cell ran once with
the frozen nine-variable environment, release profile, default features
disabled, only `liteinst-after-loader-experiment` enabled, the exact test name,
`--exact --nocapture --test-threads=1`, and no retry. Available space before
the matrix was 923 GiB (required floor: 400 GiB).

## Results

| Cell | Result | Log SHA-256 |
| --- | --- | --- |
| `command_environment_mismatch_is_typed_and_never_enters_the_guest` | PASS, Cargo exit 0 | `3dcb29790f6c191b5c21d9e42a508488a22a5a4c2b27010b1da6ef10b1e3bffd` |
| `stats_output_api_matches_old_api_raw_bytes_for_four_calls` | FAIL, Cargo exit 101 | `d90119b08a8e266dbbbe9a48d7aeff797d92c982ac6ff5c6553d02c0091bc362` |
| `installed_hook_timer_transport_deopts_to_exact_ptrace_trajectory` | FAIL, Cargo exit 101 | `8e337fa576032fc81b04f3a4e8bf97da90be56ef8cd5a56d1ece0c28518b855d` |
| `non_output_stats_api_reports_four_call_dispatch_exactly` | FAIL, Cargo exit 101 | `10acfdb2aa1eff591e20e209196598f81ab20460c17a31b49cb28c0168c59028` |
| `one_getpid_call_has_no_direct_hook_or_fallback_dispatch` | FAIL, Cargo exit 101 | `4c8c08448d330fed1222f8a86dae74da1a778cfce72d951b2678f044e5199943` |
| `unpatchable_getpid_refuses_installation_and_uses_one_retained_fallback` | FAIL, Cargo exit 101 | `5ec3bdb39a8894cf0b1bfd035aad412372cb9851b177767c37543ef84a89cddf` |

The middle three failures reached the same bound-libc `brk(0x426000)` growth
after the initial current break was bound as `0x405000`; the private policy
modeled a query but not a successful break transition. The final two failures
reached the runtime initializer and refused its exact
`openat(AT_FDCWD, "/proc/self/fd", O_RDONLY|O_DIRECTORY|O_CLOEXEC)` audit
before any tested LiteInst call. These are candidate defects, not assertion or
comparator failures, and no failed cell was relabeled or retried.

## Post-run frozen artifact hashes

```text
e71a87451a988c09cda15d17e75f01292119426d65c2a507d64bd8deb3dd82d7  stage/after-loader-fixture
462609264749a591d93efdff6f596d6337d6930b960c948545250ed16016205f  stage/libreverie_liteinst.so
40e4f4b87a0135a1b123ee1f624067494150a2f8a9f12cccf94c6d27e3fc3cf3  stage/runtime.marker
3d2c470197194236a9da9274f98a4333691f095602d72dd8f1b4a29d1c83160c  stage/four-canonical.manifest
7c886ac99e5a54ea4852b055422654b058a998f735c2b35b9f46e2cc7909484d  stage/one-call.manifest
c7fa200d3b3705f3eb36e7271a7b8b7387e9c715a51f0a468811a25c12c4ce35  stage/unpatchable.manifest
```

All six hashes are byte-identical to the pre-run receipt. A corrected
candidate requires fresh source review, staging, artifact review, and explicit
execution authorization; this failed packet is not reusable for a retry.
