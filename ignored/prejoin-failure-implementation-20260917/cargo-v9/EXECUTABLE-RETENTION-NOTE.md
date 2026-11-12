# Executable retention clarification

The source-v12 cargo-v9 native result was verified against the actual emitted ELF SHA 0c3d0714c52d945444fb1e0b34abfe75cc61ea91ecc1ef6d814b3072bd3bf3a1 before and after its 37-test run. The raw compiler artifact, launch, outputs, accounting and independent hash verification remain retained. The ELF bytes themselves were not copied out of the owned Cargo cache before the subsequent authorized v13 build reused that cache and replaced its path. The old SHA is observed execution evidence, not a claim that the v12 ELF bytes are still present.

The two actual v13 qualification-build-v1 ELF files have now been copied into its run-1/retained-elf-v13 directory with complete size, mode and SHA readback before any v14 cache reuse. No v13 binary is relabelled as v12 or v14.
