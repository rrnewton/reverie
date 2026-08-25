# ci-hub change clone preservation and disposal record

Search root: `/home/newton/temp`

Final verification:

- All 11 exact clone paths are absent.
- No `ci-hub-change-manifest.json` remains under `/home/newton/temp`.
- The fork ref `refs/heads/ci-hub/extend-ci-hub-centralized-validate-to-reverie-with-its-own-ledger` was rechecked after disposal and still resolves to `7587470dafb5ff15336adfb76cdc8ca1e614c005`.

## Preservation records

- Path: `/home/newton/temp/dev-hermit-reverie-validate`
  - Had work: yes
  - Preserved SHA: `7587470dafb5ff15336adfb76cdc8ca1e614c005`
  - Verified remote ref: `refs/heads/ci-hub/extend-ci-hub-centralized-validate-to-reverie-with-its-own-ledger`
  - Remote: `https://github.com/rrnewton/dev-hermit.git`
  - Verification: `git ls-remote` returned the exact SHA above
  - Deleted: yes (`dispose` reported success)

## Per-clone results

- Path: `/home/newton/temp/dev-hermit-2163-local-authority`
  - Had work: no (`unpushed=0`, `uncommitted=0` after remote refresh)
  - Pre-deletion HEAD: `eac788d7b5706702e8d47223f775ba0b4610b965`
  - Pre-deletion recovery ref containing HEAD: `refs/remotes/origin/main`
  - Deleted: yes (`dispose` reported success)
- Path: `/home/newton/temp/dev-hermit-bind-registered-pr-close-codex-hermit4-014`
  - Had work: no (`unpushed=0`, `uncommitted=0` after remote refresh)
  - Pre-deletion HEAD: `87358522a0918bbe64ee8e9f3c2ca93c34280b4f`
  - Pre-deletion recovery ref containing HEAD: `refs/remotes/origin/main`
  - Deleted: yes (`dispose` reported success)
- Path: `/home/newton/temp/dev-hermit-ci-dag-jobs-propagation`
  - Had work: no (`unpushed=0`, `uncommitted=0` after remote refresh)
  - Pre-deletion HEAD: `9350e674afb7bdc67ddb769041ab75e38727b745`
  - Pre-deletion recovery ref containing HEAD: `refs/remotes/origin/main`
  - Deleted: yes (`dispose` reported success)
- Path: `/home/newton/temp/dev-hermit-implement-land-lock-reap-fix`
  - Had work: no (`unpushed=0`, `uncommitted=0` after remote refresh)
  - Pre-deletion HEAD: `c68d92705d1c2f5f197897d7077b51227ffc01bc`
  - Pre-deletion recovery ref containing HEAD: `refs/remotes/origin/main`
  - Deleted: yes (`dispose` reported success)
- Path: `/home/newton/temp/dev-hermit-land-who-fix`
  - Had work: no (`unpushed=0`, `uncommitted=0` after remote refresh)
  - Pre-deletion HEAD: `ada99800ff48df863db09b7bd6abca3a245958ea`
  - Pre-deletion recovery ref containing HEAD: `refs/remotes/origin/main`
  - Deleted: yes (`dispose` reported success after retrying a host file-access refusal)
- Path: `/home/newton/temp/dev-hermit-locally-validated-reconcile`
  - Had work: no (`unpushed=0`, `uncommitted=0` after remote refresh)
  - Pre-deletion HEAD: `8fe5fc568cd1a88d070028bee49f2f84816bbbc8`
  - Pre-deletion recovery ref containing HEAD: `refs/remotes/origin/main`
  - Deleted: yes (`dispose` reported success)
- Path: `/home/newton/temp/dev-hermit-obligation-late-receipt`
  - Had work: no (`unpushed=0`, `uncommitted=0` after remote refresh)
  - Pre-deletion HEAD: `ed7c12486cdb9cefdf20e076accd4e204f6ff6a9`
  - Pre-deletion recovery ref containing HEAD: `refs/remotes/origin/main`
  - Deleted: yes (`dispose` reported success)
- Path: `/home/newton/temp/dev-hermit-pr-status-population-cache`
  - Had work: no (`unpushed=0`, `uncommitted=0` after remote refresh)
  - Pre-deletion HEAD: `028b3cdf3dd593632cc3f6902a87180419e4b7c2`
  - Pre-deletion recovery ref containing HEAD: `refs/remotes/origin/main`
  - Deleted: yes (`dispose` reported success)
- Path: `/home/newton/temp/dev-hermit-reverie-validate`
  - Had work: yes; preserved as recorded above (`unpushed=0`, `uncommitted=0` after preservation and remote refresh)
  - Pre-deletion HEAD: `d8fc345828ef4a4b9c6ce2adedafd63da2d57502`
  - Pre-deletion recovery ref containing HEAD: `refs/remotes/origin/main`
  - Preserved SHA: `7587470dafb5ff15336adfb76cdc8ca1e614c005`
  - Verified remote ref: `refs/heads/ci-hub/extend-ci-hub-centralized-validate-to-reverie-with-its-own-ledger`
  - Deleted: yes (`dispose` reported success)
- Path: `/home/newton/temp/dev-hermit-soft-green-lander`
  - Had work: no (`unpushed=0`, `uncommitted=0` after remote refresh)
  - Pre-deletion HEAD: `a78f0a5ab81e044e85d10fde9d4f13ac6cfe9ad0`
  - Pre-deletion recovery ref containing HEAD: `refs/remotes/origin/main`
  - Deleted: yes (`dispose` reported success)
- Path: `/home/newton/temp/dev-hermit-validate-checkout-lifetime`
  - Had work: no (`unpushed=0`, `uncommitted=0` after remote refresh)
  - Pre-deletion HEAD: `ea96d3a28523af5cd0fb782c95863b0d7c3ec134`
  - Pre-deletion recovery ref containing HEAD: `refs/remotes/origin/main`
  - Deleted: yes (`dispose` reported success)
