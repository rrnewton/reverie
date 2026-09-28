# Diagnostic bounds wording correction

Earlier pre-join reports and messages used “uncapped” to describe successful diagnostics. That wording was incorrect. The actual observer applied 1 MiB or 16 MiB byte caps, as recorded per stage. The correct description is **bounded and untruncated**: the configured caps remained active and were not reached. This correction preserves the existing reports and all plans that bind their original bytes; it changes no measured result, selected population or bound.

Successful per-stage source/input assertions are evidence that the bound identities matched at those checkpoints. They are not separately retained full source snapshots taken after every stage. The frozen source manifests and exact input records remain the source of those comparisons.
