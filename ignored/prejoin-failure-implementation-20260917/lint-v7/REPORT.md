# Reverie source v16 lint result

Both released read-only checks passed on frozen source v16. Workspace cargo fmt --all -- --check used 1.631580 CPU / 2.151952383 wall seconds. Default-feature cargo clippy --locked --offline -p reverie-kvm --all-targets -- -D warnings used 8.191205 CPU / 5.384520527 wall seconds. Full stdout and stderr were read: formatting emitted nothing, Clippy emitted only its ordinary check/finished lines, with no warning or error.

Both observed services had complete CPU accounting, no bound or observer failure, uncapped diagnostics, and inactive/empty terminal state verified independently. Full source, lock and all bound inputs matched afterward. Actual launch SHA256 is c809c5f81540b841f79c1828402288cad0fcb06672ba7fdbe7254f46168fb5bb. Raw observer results and independent service readbacks remain under run-1 and the exact plan output paths. These lint results execute no native, VM or guest test and do not replace the separate qualification obligation.
