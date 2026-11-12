# Source v19 formatting and default-feature Clippy

Both released read-only checks passed on frozen source v19. Workspace formatting consumed 1.649286 CPU / 2.168266367 wall seconds. Default-feature `cargo clippy --locked --offline -p reverie-kvm --all-targets -- -D warnings` consumed 4.938099 CPU / 3.176783058 wall seconds. Full stdout/stderr were read; no warning or error diagnostics were emitted. Outputs are bounded and untruncated.

Both observed services completed with full accounting, empty cgroups, and independent inactive/dead, MainPID 0, empty ControlGroup readbacks. The complete source/input comparison passed after both checks; no separately retained complete after-snapshot is claimed. Earlier failed/unexecuted checks and every previous source packet remain preserved. This is lint evidence only, not VM or guest qualification.
