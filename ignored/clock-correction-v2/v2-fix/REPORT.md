# Two-import correction after the preserved compiler failure

This source-only successor fixes exactly two test imports in injection_stop_tests.rs: use the public reverie::syscalls::PathPtr re-export, and remove the unused std::io::Write import. The existing formatter places PathPtr with the other public syscall imports. The public export is verified from the bound reverie-syscalls/src/lib.rs. Every production byte, test body, assertion, guest byte sequence, five-second deadline and other source entry remains identical to the first candidate.

The first run remains metadata raw0/accepted and compile raw101/refused, with E0603 plus the unused import warning. All 38 declarations were unrun. Its raw streams, source, completed private token and final evidence are preserved. This successor is unexecuted, and it claims no clock result or approval.

SOURCE.patch is the full change against exact3d4/tree730, also landed b5's identical tree. V2-FIX.patch shows only the import correction against W/source. The original author report retains the precise post_exec0/timer1 rationale and real1/65 or65/129 requirements. Seven new declarations still cover eight modes; Write checks only its three meaningful arguments. No selection or coverage claim expanded.

The fresh caller is W/qualification-v2, alongside qualification-v1, so all 11 runtime and orchestration helper files remain byte-identical. Only data bindings and owned source/target/output paths change. There are still 45 observed phases and 38 declarations under the original limits. Both new harnesses must be emitted fresh=false and retained; no failed predecessor target or ELF is reused. Author formatting is separately recorded, not qualification. No metadata, compiler, test, guest or lease admission has run for this successor.
