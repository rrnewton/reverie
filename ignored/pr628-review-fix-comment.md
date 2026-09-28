[hermit2, degraded-unresolved, gpt-5.6-sol, devbig014, role=impl]

External-review correction is ready for fresh exact-head review.

- Previous reviewed head: `730123acb922cfa30fa1bb9281391776088fb35d`
- New head: `5d65b9f50f124f615c5f0c862af7b94919037c3f`
- Base: `7bc49f4c4d63018adba61d49246e847569518e33`
- Full diff SHA-256: `2b8dbdfb356d055e3573aecc23cd0dbb02f0a4c0bb0e74b2f75b71febdd6f88a`
- Correction-only diff SHA-256: `c1847753c83977877f2d7ff5186b9f4a2c5f2625f3e73bab4bfd8b5749380dfc`

The fix rejects a negative decoded `sendfile` output fd before ordinary input classification can return the KVM `ENOSYS` fallback. Unit coverage crosses `0x5a5a5a5a80000001` and sign-extended `-1` with pipe, socket, directory, stdout, and real procfs inputs. The real-KVM fixture crosses both encodings with pipe and AF_UNIX socket inputs using `offset=NULL` and count 1.

Exact-head evidence:

- fmt and all-target clippy with `-D warnings`: green
- new focused regression and original focused low-word unit: green
- real KVM: native once, direct twice, Tool twice; exact output/exit/status parity; green
- full library parallel: 818/818
- full library serial: 818/818
- removing only the new guard makes both unit and KVM tests fail (`ENOSYS` instead of `EBADF`; KVM exit 22)
- two independent Codex reviews approve this full diff with no goalpost moving

Disclosed residual: the pre-existing private synthetic `/proc/*/fdinfo` pre-dispatch refusal still precedes `sendfile`; this patch's procfs row exercises ordinary host procfs and makes no private-fdinfo ordering claim.

Merge remains held pending a fresh coordinator-relayed Claude-family verdict on the exact new head and diff above.
