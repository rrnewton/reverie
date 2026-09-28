# Source finding before execution

V7 is retained as unexecuted preparation, not an accepted implementation.
Final source inspection found that the current shared reader does not enforce
equal compared-log counts. `VerificationReport::require_canonical_comparison`
at immutable 8051335e `hermit-cli/src/canonical_verdict.rs:553` requires positive
left/right counts. `require_canonical_match` at line 618 adds exact available
output operands and matched/verified/bitwise claims, but not count equality.
The new v7 unequal-count negative would therefore expose an existing reader
limit. This is a source-derived prediction; no test or product run occurred.

Consequently, v7 REPORT's statements that the existing reader already enforces
equal log counts are incorrect. Its broad statement about rejecting filtered
evidence must also be read narrowly: the current method validates canonical
strictness, enabled log comparison, a canonical record envelope, nonempty
counts, output evidence, and the match claim. It is not a validator of every
optional comparison-policy description field. No execution evidence supports
a broader claim here.

The next isolated successor will add equal-count admission to the typed
canonical-match method and retain the prepared actual-reader negative. Parsing
and canonical divergence admission must remain unchanged; a distinct direct
control will preserve historical unequal-count bytes while refusing their
claimed canonical match. This must not be implemented as another handwritten
JSON acceptance predicate in the shell callers.

The concrete reader packaging limitation and current matrix policy/label
obligations in REPORT-v7 remain valid. Earlier versions and this first
source-derived finding are preserved rather than silently rewritten.
