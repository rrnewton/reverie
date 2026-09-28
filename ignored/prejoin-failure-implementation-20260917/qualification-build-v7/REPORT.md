Final committed 12d4ce8c checks passed.

Compile and complete actual inventories only. No test or VM execution.

- compile: 21.760329 CPU seconds, 10.537082949 observed wall seconds; payload exit 0, complete accounting and two fresh inactive/empty readbacks.
- list-lib: 0.215416 CPU seconds, 1.029580217 observed wall seconds; payload exit 0, complete accounting and two fresh inactive/empty readbacks.
- list-static-elf: 0.265595 CPU seconds, 0.997331147 observed wall seconds; payload exit 0, complete accounting and two fresh inactive/empty readbacks.

Actual inventories: 454 library methods (all 453 retained plus the one new eight-case control), 288 static integration methods unchanged. No structured compiler-message diagnostics. Both emitted ELFs have separate byte-verified retained copies.

All 75 explicit inputs and the full 2,596-entry source manifest still match. Output is bounded and untruncated. Earlier failures remain separate.
