# LiteInst __vdso_getrandom revision 02

Task: correct all findings from independent review 01 of the frozen query-preserving proposal.

Base: 526c21cf06ef9e5098ec9002b93e40e2022e798f
Slot head before edits: c8f4ca9d2e95460e027678ff23f6dec2529d255d

Constraints: source correction only; no build, test execution, candidate machine-code execution, Hermit, staging, commit, push, or other SCM mutation.

Revision 01 and its independent review are immutable evidence.

Revision 02 is frozen at:
/home/newton/work/dev-hermit/ignored/liteinst-01a0a13c-review/queue-drain-20260916/liteinst-vdso-getrandom-query-safe-02

Exact review bindings:
- FREEZE.json: bfeaa0ab6b839b6d4c989be98898aa38f364a031ad7ebfb98016b5c9af075e8f
- proposed.patch: d323668b081b2071a501abe5464ef054fbfb16f799f6a3b72396f372e3d84d9b
- candidate reverie-ptrace/src/vdso.rs: 8ca14973d69045d959db764972e15c04f8525edce21057d4c516b3adf6b38be4
- candidate reverie-liteinst/src/runtime.rs: 3a0d8cd279bdac511b7208bbc5f5c2d2df91aa58780939813957d064cd2d5a95

Only rustfmt parsing/static checks, diff checks, byte/hash checks, and a read-only patch applicability check ran. All candidate execution remains prohibited pending independent review.
