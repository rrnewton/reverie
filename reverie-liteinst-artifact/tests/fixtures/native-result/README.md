These are normalized fixtures derived from one ordinary successful build capture
on 2026-10-25, using installed Buck revision
`f41e47335a0719274872e02761f283b64c335477`. The source packet that explains
serializer policy is pinned separately to
`13f223c4320d5a78da497e7dde6c25ab2a728eec`.

The original packet seal SHA256 is
`d3fd14e94879573d7a699069cf8eb46b9ead0ed2f6e0e9b583498f764bef095e`.
The original event stream's decompressed SHA256 is
`88c5befa4a68917b165bdac4d2500e52278c7681b57b90efba20d3ca260c9e52`.
Selected raw JSONL records were lines 63710 (action end), 63711 and 63712
(materialization start and end), and 63725 (final result).

Only project, target, configuration, artifact-path and hostname string identities
were replaced with generic examples, plus JSON indentation. Field names,
wrappers, nulls, values of other fields and array ordering are preserved. These
files are not byte-identical original records or a build of the generic example.
The requested export target's configuration remains distinct from the output
producer's configuration. `default_info` is a response boolean, not a projection
rule for named report providers.

The action has `commands: []`, zero wall time and numeric `execution_kind: 10`.
The installed revision's enum mapping is not established by the older source
packet. No recipe is present, and no execution-kind interpretation supplies one.
A successful parser test proves only these observed wire shapes and the refusal
to extract a missing recipe. It provides no source/action/input binding,
qualification, current file custody, physical proof or guest-execution result.
