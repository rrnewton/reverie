# Provenance

Written by the agentcloud worker `sub-crashcortex` on an OnDemand fbsource node, 2026-09-29T21:08Z to about 2026-09-30T01:35Z. The node's GitHub egress was blocked, so the worker saved everything to Phabricator paste P2528653214 (25,464 bytes, read back byte-identical by the worker) before its node lease expired.

coord2 read the paste on devbig030 with `meta phabricator.paste read --id P2528653214` and split its `===== <name> =====` sections into the files in this directory, byte for byte, except for one change: one internal fleet host name in REPORT.md (a `--representative-host` argument) was shortened to its first DNS label, as the tracked-evidence redaction rule requires.

The raw run artifacts and transcripts (`/data/users/newton/scratch/cc-retry-20260929/` on that node) were lost when the lease ended. These files and the six diffs' test plans (D116819880, D116846891, D116857431, D116858160, D116988175, D117035583) are the durable record.
