---
name: benchmarking
description: "Protocol for reproducible scientific benchmark artifacts: provenance, methods, evaluation, results, same-host comparisons, short hostnames, and full-SHA metadata. Load when planning, running, reviewing, or publishing benchmark work."
---

# Benchmarking

Use this protocol for every durable performance experiment at its task-owned
location. Parent experiments use `experiments/<name>_YYYYMMDD/` as defined by
`AGENTS.md`; product experiments stay in their product repository. A result is
not publishable until another engineer can tell
what ran, reproduce it, and distinguish measured facts from interpretation.

## Required writeup

The experiment README must contain these sections:

1. **Provenance**: UTC run date, source repository and short revision, short
   machine name, runtime/tool versions, run IDs, and a pointer to
   `metadata.json` for exact details.
2. **Methods**: exact invocation, provisioning, native baseline, changed
   variable, execution order, warmups, measured repetitions, statistic,
   timeout, correctness gates, machine constraints, and where raw artifacts
   live.
3. **Evaluation**: what every benchmark actually does, which resource or
   mechanism it stresses, and workload/duration intuition. Do not assume a
   benchmark name is self-explanatory.
4. **Results**: absolute measurements, normalized values where useful, sample
   counts, failures/timeouts, top-line qualitative meaning, uncertainty, and
   limitations. Separate measurements from conclusions.

Keep the README concise enough to review. Put full rows in TSV/CSV/JSON and
link them from the prose.

## Experimental validity

- Compare identical workload inputs and commands. Change only the independent
  variable being studied. If coverage, flags, job bounds, or workload versions
  differ, mark the rows unranked.
- Normalize only to a baseline from the same collection. Never claim that a
  local system beats a number measured on another host. Cross-host results may
  be historical context, not a ranking.
- State semantic differences. Instrumentation, determinization, sandboxing,
  virtualization, and native execution do not provide interchangeable
  guarantees even when they run the same command.
- Rotate or randomize execution order when practical. Record the policy and
  host load. Use enough warmups and repetitions for the workload cost, and say
  explicitly when an expensive result is only one sample.
- Gate every timed sample on exit status and workload-specific correctness.
  Keep failed and timed-out rows; never drop them or summarize them as fast
  measurements.

## Hostname privacy

Persist only the first DNS label, such as `buildhost42`, in Markdown, JSON,
TSV, CSV, logs selected for check-in, and generated reports. Never persist an
internal FQDN. Normalize hostnames in the runner before writing artifacts; do
not rely only on a post-run editorial scrub.

Before publication, scan the complete experiment, including metadata and data
tables, for dotted internal hostnames. Treat any hit as a policy failure.

## Metadata and SHA convention

Every result directory has a parseable `metadata.json`. Use full immutable
identifiers there:

- `repository_sha`: full 40-hex commit for the harness checkout;
- `hermit_sha`, `reverie_sha`, and other product SHAs when those products
  contribute to the result;
- `script_sha256`: SHA-256 of the exact producing runner;
- source/input SHA-256 or SHA-512 values and immutable URLs/digests for fetched
  artifacts;
- run ID, UTC date/time, short hostname, kernel/hardware facts, dirty-state
  indicator, workload parameters, seeds/order, warmups, repetitions, timeout,
  and tool versions.

Use a short Git hash only in human prose. The metadata keeps the corresponding
full commit. A later documentation or runner edit does not change an old
result's `script_sha256`; that hash identifies the producer that generated the
numbers. Rerun the benchmark to claim measurements from a new producer.

## Artifact policy

Track runners, small text results, metadata, and summaries. Keep binaries,
images, extracted roots, profiles, cores, and raw high-volume logs under an
ignored directory. Fetch large inputs from immutable URLs with verified
content hashes rather than vendoring blobs.

## Publication checklist

- Parse every JSON file and verify every TSV/CSV row has the declared schema.
- Recompute summaries from raw tracked rows and check uniqueness of the result
  key.
- Run formatters and static checks for the runner language.
- Verify the short-host/FQDN scrub across the entire artifact.
- Verify source revisions, producing-script hashes, input digests, row counts,
  correctness gates, warmups, repetitions, and timeouts agree between prose
  and metadata.
- Inspect the exact staged diff and publish only the experiment-owned paths.
- Use `with-proxy` for network provisioning, GitHub, fetch, and push operations.
