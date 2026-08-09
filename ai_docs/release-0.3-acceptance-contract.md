# Hermit 0.3 executable acceptance contract

The authority is the 26-criterion machine-readable contract in
`ai_docs/release-0.3-acceptance-contract.json`, evaluated only by the standalone
`scripts/release-0.3-acceptance.py`. This contract does not query a label, status cache, receipt, or validation
ledger.

The sole previously verified green boundary is Hermit
`d53550510d1e7d13e84cc8af9bb90269e90b3f07`. That baseline never substitutes for validation at the tested RC
SHA. That is a Hermit boundary, never a parent-repository boundary. This task's working-tree contract diff is
based atop parent SHA `ec33089a26e0270464cf53092f13debf29243482`; parent main is unverified. The report carries
`verified_hermit_base_sha`, `tested_rc_sha`, `parent_contract_base_sha`, and `parent_status`. When the tested
Hermit RC is current main, differs from the Hermit boundary, and lacks an exact manual pass, it says
`green at d5355051, current main unverified`.

## Decision command

```sh
python3 scripts/release-0.3-acceptance.py \
  --evidence PATH/TO/evidence.json \
  --parent-sha PARENT_40_HEX \
  --hermit-sha TESTED_RC_40_HEX \
  --reverie-sha REVERIE_40_HEX \
  --json
```

Read `decision`, `verified_hermit_base_sha`, `tested_rc_sha`, `tested_rc_status`, `parent_contract_base_sha`,
`parent_status`, the three counts, and every
`criteria[].{id,verdict,reason,command,fields,denominator,threshold,forbidden,runner,signer}`. `GO` requires
`PASS=26/26`, `FAIL=0`, and `NO_RESULT=0` at one exact parent/Hermit/Reverie tuple.

- Exit `0`: `GO`.
- Exit `2`: `DEPLOYMENT_DEFECT`; a criterion, named runner, or named signer does not exist. Release is forbidden.
- Exit `3`: `FAIL`; a threshold failed or a forbidden state was observed. Release is forbidden.
- Exit `4`: `NO_RESULT`; evidence is missing, partial, stale, zero-count, unauthenticated, or malformed. Release is
  forbidden.

Every manifest item and every raw JSON artifact must carry the full exact parent/Hermit/Reverie tuple. Every
manifest also records the contract registry's runner and signer identities. The checker recomputes each raw
file's SHA-256 for byte integrity; a digest alone does not authenticate provenance. A tuple mismatch, including
one caused by a rebase, is `NO_RESULT`.

## Exact-RC manual validation gate

From an exact clean Hermit slot at `tested_rc_sha`, run sequentially and bypass all caches:

```sh
CI_DAG_JOBS=1 ./scripts/validate.rs full --ignore-cache --no-label-pr --verbose 2>&1 | tee "$RAW_LOG"
status=${PIPESTATUS[0]}
printf '%s\n' "$status" >"$EXIT_FILE"
```

`$EXIT_FILE` must contain `${PIPESTATUS[0]}` captured immediately after the pipeline, not `tee`'s exit status.
The checker reads and hashes both raw files. `AC-STRICT-03` passes only if the log contains all of:

- exact `Commit: <tested_rc_sha> (clean tree, commit-anchored); selection: full`;
- `Validation level: full`;
- a successful final `Validation summary [full] (... passed, 0 failed...)`;
- pipeline exit `0`;
- a sum greater than zero across raw `test result: ok. N passed` lines;
- no `CACHE HIT`, interruption/cancellation, environmental block, or incomplete marker.

Any missing condition is `NO_RESULT`, except a nonzero pipeline exit is `FAIL`. No historical baseline, label, or
copied green can satisfy this gate.

## Hosted readiness and tier gates

`AC-PRE-01` and `AC-PRE-02` consume raw named-job JSON artifacts obtained directly with the recorded `gh run
download ...` command. Each job must carry its exact head SHA, run ID, job ID, status, and conclusion. The parent
set is exactly four named jobs: `4/4` passes and `3/4` is `NO_RESULT`. If no authenticated raw artifact exists,
the gate remains `NO_RESULT`; a copied status is not accepted.

`AC-STRICT-01` states its cell denominator explicitly. Every `SHORT` cell requires stdout, nonzero INFO, stack,
and heap parity. Every `LARGE` cell requires stdout and nonzero INFO plus a current exact-pair stack/heap
spot-check. A tier name without those fields is `NO_RESULT`. `AC-RUNNERUP-01` derives “more than half” from the
declared denominator; no evidence-supplied half-denominator is trusted.

The JSON contract is the nonduplicated table of all 26 commands, fields, denominators, thresholds, forbidden
states, runners, and signers. Print it with:

```sh
jq -r '.criteria[] | [.id, (.command|join(" ")), (.fields|join(",")),
  (.denominator|tojson), (.threshold|tojson), (.forbidden|tojson),
  .producer.runner, .producer.signer] | @tsv' \
  ai_docs/release-0.3-acceptance-contract.json
```

## Mutation bracket

```sh
python3 scripts/test_release_0_3_acceptance.py
```

The positive control accepts a legitimate `26/26`. Negative controls plant zero raw tests, an incomplete `3/4`
job set, a rebased SHA, missing runner and signer authorities, wrong raw digest, incomplete short and large tiers,
a cache hit, and each of the 26 missing artifacts. The controls therefore prove both refusal and acceptance.
