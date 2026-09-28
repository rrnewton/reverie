The single released source-v3 sequence passed compilation, listing and all37 selected native methods (zero failed or ignored). This includes the strengthened pipe-close/reuse control and the existing executable-link replacement/deletion test. Prior v1 E0061 compilation failure and v2 native35pass/1fail remain retained, including the v2 actual executable. No automatic retry or source change occurred.

compile: exit0, 7.408433 aggregate CPU seconds / 5.124889399 wall seconds.
list: exit0, 0.202079 aggregate CPU seconds / 0.996965096 wall seconds.
native: exit0, 0.228809 aggregate CPU seconds / 1.016614923 wall seconds.

All three services have complete accounting and fresh inactive/dead/MainPID0/empty control-group readbacks; no observer or bound failure. The actual test executable is 106866600 bytes, SHA256 b899a2eec14c966aa64dcadd2064177cd412e3a2e57abe68efbf2e327075a783. All38 plan inputs,2550 source files, six candidate modes and the unchanged Cargo.lock were independently read back.

Complete37 individual outcomes and exact bindings are in execution-readback.json, SHA256 e8451672e5173204681400deae70af1db6e7e87d8701c300403f00dbfe110626. Native coverage does not establish Hermit guest success, original closed-stdin completion or backend parity. Clippy and product publication are separate.
