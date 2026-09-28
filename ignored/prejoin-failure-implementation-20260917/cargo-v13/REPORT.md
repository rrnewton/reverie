# Source v19 native controls

Cargo-v13 compiled source v19 and passed all 40 selected native controls, with no failures or ignored tests and 409 filtered out. Actual library inventory is 449: all prior 448 identities plus the single new peer-cancellation control. That method checks eight cases through the production RPC driver, retirement helper and consuming finisher. It does not itself create a VM or an OS worker; the original real integration cohort remains required.

Compilation consumed 7.830975 CPU / 5.179857066 wall seconds; inventory 0.217705 / 0.907460391; native execution 0.236380 / 1.055218177 (libtest reports 0.22 seconds). Structured compiler stdout contains zero compiler-message diagnostics. The full native stdout and stderr were read; expected forced panic and registry-poison diagnostics from existing controls are retained. Outputs are bounded and untruncated.

All three actual services completed with full accounting and independent inactive/dead, MainPID 0, empty ControlGroup readbacks. Complete live source/input and emitted executable comparisons passed after execution; these are comparison assertions, not separately retained complete after-snapshots. The actual native ELF is copied on a separate inode under run-1/retained-elf-v19/lib, SHA 281cb551a07d1b255dfa3757a166b401f82769ce321390d93753d1de1a11a6dc (115730656 bytes, mode 0755). Earlier v16 and qualification ELF copies remain preserved.

V18's first compile failure and its unexecuted later stages remain intact. This passing native population is not actual VM, original static_elf, Hermit guest or cross-backend parity evidence.
