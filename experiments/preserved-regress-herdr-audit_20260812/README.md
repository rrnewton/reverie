# Preserved `regress` Herdr audit record

This directory preserves the only uncommitted generated record found at
`worktrees/regress/reverie/.herdr-run/audit.jsonl` during the idle-slot
durability census. The generated `.herdr-run` file does not belong in the
Reverie product repository, so this curated parent experiment records its full
contents and provenance instead.

- Source slot: `regress`
- Source repository: `reverie`
- Source checkout SHA: `dd3c178ea9553004d7bf4c494e1b7fd80e7b6ae6`
- Original byte length: `327`
- Original SHA-256: `9caa979fd7ec6433d308f4cfdf7f02346aa0e88c707211c5a1755892c1838542`
- Captured at: `2026-08-12T15:22:20Z`

Original JSONL record:

```json
{"agent": "hermit-regress", "command": "cd /home/newton/work/dev-hermit/worktrees/regress/reverie && with-proxy git push origin HEAD:refs/heads/fix/kvm-tree-root-identity-vs-guest-ppid", "detail": "program 'cd' is not allowlisted. Allowed: cargo, gh, git", "pid": 3024086, "time": "2026-08-06T13:27:35Z", "verdict": "REFUSED"}
```

This is preservation evidence, not a product result or validation claim.
