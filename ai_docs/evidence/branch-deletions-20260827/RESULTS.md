# Branch deletion results

Deletion completed on 2026-08-27. All commands used the exact SHAs recorded in
`deleted-branches.tsv` as `--force-with-lease` expectations and used an atomic
push for each repository. No lease refused a deletion.

The remote was read after each push. All 353 recorded branch names were absent:

| Repository | Recorded | Deleted | Recorded names remaining |
| --- | ---: | ---: | ---: |
| `rrnewton/hermit` | 138 | 138 | 0 |
| `rrnewton/reverie` | 197 | 197 | 0 |
| `rrnewton/dev-hermit` | 13 | 13 | 0 |
| `rrnewton/agent-utils` | 0 | 0 | 0 |
| `rrnewton/liteinst2` | 5 | 5 | 0 |
| **Total** | **353** | **353** | **0** |

The remote branch counts changed from 3,708 to 3,355, exactly the 353 recorded
deletions. The post-deletion counts were:

| Repository | Remote branches | `refs/rescue/*` | `archive/*` | Main SHA |
| --- | ---: | ---: | ---: | --- |
| `rrnewton/hermit` | 2,267 | 1,137 | 46 | `e2d89097f1baa8af260c68994201121ac4c557da` |
| `rrnewton/reverie` | 292 | 44 | 28 | `0cc049fa68c0628024d1329b168edf9d8398d267` |
| `rrnewton/dev-hermit` | 744 | 431 | 278 | `affcd778ef83eb3878704232d3ff340d9042184e` |
| `rrnewton/agent-utils` | 39 | 37 | 0 | `ff3b5e28c7e58d81a1d0e5f772b3c84765a6dbd6` |
| `rrnewton/liteinst2` | 13 | 1 | 0 | `ac6df266c76dcd5c724df958321ff4f9c0b23f63` |

The protected totals remain 1,650 `refs/rescue/*`, 352 `archive/*`, five
`main` branches, and no `integration/*` branches. The 1,348 branch tips without
sufficient content proof were not included in any deletion command. No pull
request was closed.

An initial Hermit command was assembled with the remote argument in the wrong
position and failed locally. A remote read before retrying showed all 138 names
still present at their recorded SHAs. The corrected command then performed the
atomic leased deletion reported above.
