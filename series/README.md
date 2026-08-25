Published stress/flakiness series shards: `series/<team>/<short-host>/<YYYY>-<MM>.jsonl`.

Append-only. Live producers write `ignored/ci-hub/series-spool/` and never these
files; readers union the spool with every shard here. See `ci-hub/series/README.md`.
