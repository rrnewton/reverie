# Source v19 qualification build

The released qualification-build-v4 compiled the frozen Reverie library and original static_elf integration target, then recorded actual inventories of 449 library methods and 288 static_elf methods. All 40 selected native identities, four separate VM identities, and 22 original static methods are present. This attempt executed no tests or guests. Structured compiler stdout contains zero compiler-message diagnostics; complete stderr contains only ordinary compile/finish messages. Raw outputs are bounded and untruncated.

compile: 15.124801 CPU / 9.283845666 wall seconds, service `safehermit-20260917T161310Z-4037346.service`.

list-lib: 0.215409 CPU / 0.870591733 wall seconds, service `safehermit-20260917T161321Z-4047639.service`.

list-static-elf: 0.261766 CPU / 0.921203343 wall seconds, service `safehermit-20260917T161323Z-4051456.service`.

All three services completed with full accounting and independent inactive/dead, MainPID 0, empty ControlGroup readbacks. Full source/input and actual executable comparisons passed. Both compiler-emitted ELFs are copied to separate inodes under run-1/retained-elf-v19; exact hashes, sizes, modes and source identities are retained in its binding.json. Earlier actual ELF copies and first failures remain unchanged. Final original-cohort execution requires these exact artifacts and real admission; no native or compile result is guest parity.
