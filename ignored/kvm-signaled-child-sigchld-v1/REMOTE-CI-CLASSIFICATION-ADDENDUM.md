# Exact-head scope addendum

`REMOTE-CI-CLASSIFICATION.md` is an immutable input to the exact-head Claude
review packet and remains byte-for-byte as reviewed. Its candidate workflow ran
at `865023d1a9d2b82320a38e17e5a83bca1c4af3ee`, when the implementation patch
changed three tracked files.

The later landing candidate is the five-file head
`afd9ddc64d21967c6b1a9e203ffcd06964a4128a`, tree
`613abcec9ac50416dc36873340ed8c3547f1210a`; it adds two documentation files and
follow-up controls. The remote workflow is not exact-head coverage for that
tree. The exact-head landing evidence is the focused 11-unit plus one real
static-ELF run, 776/776 serial library tests, all-target Clippy with warnings
denied, formatting checks, and three exact-object reviews.
