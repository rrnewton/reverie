# Research-lane finding schema and trust-gate boundary

Profile: `narf-hermit-research-lane/v1`.

The three `*-findings.jsonl` files are namespaced research-lane provenance,
not the final trust-gate input:

- `N-F-*` identifies Narf-lane claims.
- `R-F*` identifies Reverie-lane claims.
- `H-F*` identifies Hermit/Linux-lane claims.

This profile governs all three research-lane files. Stable namespaced IDs allow
cross-lane dependency references without collision and intentionally deviate
from the deep-research skill's single-team `F-N` ID rule. Original
`file_ref.snippet` fields can contain three nonliteral discovery forms inside a
wider exact line range: a human-readable ellipsis; a whitespace-compacted
paraphrase; or an unmarked omission/Markdown-markup normalization. The third
form occurs in `N-F-1`, `N-F-12`, `N-F-13`, and `N-F-20`; it is recorded here
as provenance debt rather than silently described as literal. All three forms
are noncanonical discovery receipts. None may receive `VERIFIED_EXACT`,
`VERIFIED_SUBSTRING`, or any other canonical trust-gate verdict.

Before the final design can pass the trust gate, the orchestrator will create a
separate canonical `findings.jsonl` using profile
`deep-research-finding-schema/v1`: unique `F-N` IDs, literal source snippets,
exact revision/path mappings, and no ellipses, compacted paraphrases, unmarked
omissions, or markup-normalized snippets. A separate verifier will write
`verification.jsonl`; the final
design may cite only canonical IDs present in that verdict set. The original
lane files remain immutable provenance except for substantive fact corrections
requested by Audit A.

This versioned deviation does not relax the trust gate: it prevents preliminary
lane records from being misrepresented as machine-verified final citations.
