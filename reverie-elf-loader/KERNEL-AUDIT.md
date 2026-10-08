# ELF loader kernel computation audit

Reference: the Linux 6.17 source tree.
The crate is unwired: nothing in Reverie or Hermit calls it yet.

This is a source audit of the live `load_elf_binary` and `load_elf_interp`
paths and their address, size, protection, alignment and auxv computations.
It includes `maximum_alignment`, `total_mapping_size`, `elf_map`, `elf_load`,
`padzero`, the live replacements for `set_brk`, `create_elf_tables`, x64
`arch_setup_additional_pages`, mmap policy/placement, `setup_arg_pages`,
argument copying and stack alignment/randomization. Linux 6.17 has no live
`set_brk` helper: its stale comment is at `fs/binfmt_elf.c:1214`; the live
operations are per-segment `vm_brk_flags` at line 439 and final mm brk
assignment at line 1313. Those computations are audited separately.

## Reading the ledger

- **168 computation rows:** 48 main ELF/allocator rows (L), 48 mapping/helper
  rows (M), 25 argument/stack/personality rows (S), 37 table/auxv rows (A),
  and 10 vDSO/special-mapping rows (V). Some rows intentionally revisit a
  shared computation in its caller and helper; they are separately labeled.
- `K:` means `fs/binfmt_elf.c`; `E:` means `fs/exec.c`. Other kernel paths
  are relative to the reference source root. `asm/` abbreviates
  `arch/x86/include/asm/`. `C:` means
  `reverie-elf-loader/loader/loader.c`; `R:` means
  `reverie-elf-loader/src/lib.rs`. Loader assembly/linker paths are relative
  to `reverie-elf-loader/loader/`. All numeric references describe the final
  tested working source.
- Short kernel basenames are qualified by their subsection: `x86 mm/mmap.c`
  is `arch/x86/mm/mmap.c`; `process_64.c`, `process.c` and `sys_x86_64.c`
  are under `arch/x86/kernel/`; `page_types.h` is under the x86 `asm/` tree;
  `mm.h` is `include/linux/mm.h`; `hugetlbfs/inode.c` is under `fs/`.
  In the V rows, bare `vma.c` is `arch/x86/entry/vdso/vma.c`; `mm/vma.c`
  always means the separate generic VMA implementation.
- **Equivalent** is conditional on the admitted ELF, caller and host scope
  below. **Kernel-delegated** means exec of the generated template runs the
  same kernel calculation; the cited C/Rust code preserves that result or
  changes only an explicitly planned fixed-size value. It does not mean
  Rust independently implements that kernel function.
- **Refused** identifies the named preparation error or defensive runtime
  diagnostic. A native-valid refusal control is evidence for a scope
  restriction, not native/loader parity. Qualifying companions retain the
  original complete entry and assembly-observer comparisons.
- **Existing residual** identifies an already documented identity surface
  of this unwired loader; it is not claimed equal. The source/test review
  checks that its exact existing comparator has not been relaxed.

The final 29-input source manifest is
[`r5/tested-sources.json`](r5/tested-sources.json), SHA-256
`cac5b504d37e698c6890086b31bf9777d1f3426148f42b040bdab175be2cdb4d`.
The 32-file kernel reference manifest is
[`r5/kernel-sources.json`](r5/kernel-sources.json), SHA-256
`3d233288f644530520328efe0d3e1bc96287584fdb6e40a4712cadfdf8743380`.
Linux source was read only; all edits and retained artifacts are inside this
worktree. Tests ran on kernel `7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf`, rather
than a boot of the reference source tree. This audit does not certify another
kernel implementation or architecture.

## Admitted scope and existing residuals

Inputs are ordinary little-endian ELF64 x86-64 ET_EXEC/ET_DYN main programs
with PT_INTERP and exactly one nonexecuting main PT_GNU_STACK. Program-header
counts are 1..128, load addresses are ascending, loads are page-congruent,
alignments are zero/one/powers of two, and raw/effective mapped ranges are
within the documented four-level default window and above the 4 MiB loader
band. The ledger identifies every stricter geometry refusal. Dynamic
interpreters include the supported empty-first zero/negative biases; final
entry membership in an interpreter load is not required.

The caller supplies the same argv/envp, execfn byte extent, descriptor state,
personality, resource limits and namespaces, immutable pinned objects and
native exec authorization. It arranges direct binfmt_elf execution and
identical resulting credentials/secureexec for target and launcher, including
inode-sensitive LSM policy. Setid modes and file-capability presence are
actively refused; binfmt_misc recursion is outside the existing invocation
protocol. ASLR must be disabled. Finite RLIMIT_AS, insufficient/conservative
RLIMIT_DATA budgets, and inherited MDWE with executable anonymous BSS have
existing named refusals. The final README records these caller obligations.

Successful computation equivalence assumes enough kernel allocation/VMA
headroom for the launcher and its transient moves. The extra operations can
exhaust resources at a different threshold: `mm/mmap.c:376–378` and
`mm/vma.c:2775–2779` check `vm.max_map_count`; fixed `mremap` needs spare slots
at `mm/mremap.c:1039–1044,1812–1820`. C checks the returned errors at
355–356,400–403,485–488 and diagnoses them through its exit-127 path at
130–150; Rust preparation does not check this sysctl. The current host cap
is 67108864. Identical resource-exhaustion thresholds and native exec error/
status semantics are outside the existing backend failure/rollback protocol
(`README.md:62–63`); they are not claimed equivalent or preparation-refused.
No test or comparator acquires a resource-based exemption.

The host must provide readable procfs, the default 1 MiB stack guard, and an
enabled, unsealed x64 vDSO with accepted contiguous native vvar/vclock/text
geometry. Both actual host paths check `/proc/cmdline` and refuse every
visible `stack_guard_gap=` override. Missing/malformed/oversized/sealed special
mapping layouts have existing runtime diagnostics. Disabled/sealed alternate
hosts were source-audited, not boot-tested.

The exact existing procfs residuals remain: `/proc/self/exe` names the loader;
mm `start_code`/`end_code` (stat fields 26/27) name loader code; saved
`/proc/self/auxv` retains the kernel-template scalar values at mutation sites
{3,5,7,9,33}. The unchanged comparator asserts fixture-specific exact
difference sets: ordinary fixtures use {3,5,7,9,33}, and existing zero/negative
bias controls use {3,5,9} or {3,5,7,9}. Equal values, such as target PHNUM 3,
are not differences. Two known loader VMAs remain: RX loader code at
0x100000 and the shared R record at exactly 0x200000–0x201000.
Target mappings, data fields/brk, final entry state and stack auxv are compared
exactly within this scope. The harness normalizes only the 16 intrinsically
random AT_RANDOM bytes; its length/address and every other stack value remain
exact checks. No full mm/inode identity claim is made.

## Differences found and resolution

Ten finding classes were identified, including the reported round-5 defect.
Two have implementation fixes; eight have explicit new refusal classes.
There is no unresolved newly discovered address/size/protection/alignment/auxv
non-equivalence within the resulting admitted scope and caller/host conditions.

| Finding | Resolution | Mandatory controls |
| --- | --- | --- |
| Zero maximum alignment with unaligned PIE first load | Preserve maximum 0 and conditionally mask the base in Rust and C; bias becomes `0x555555554000` for offset=vaddr=`0x123`. | All-zero native/loader parity plus mixed 0/1/2/4096/2 MiB controls and independent preparation constants. |
| Rounded shadow invents padding-only anonymous BSS | Keep raw `ProgramLayout::memory_end`; emit raw shadow memsz, validate rounded brk separately and skip empty-shadow unmap. | Empty highest aligned/unaligned loads at hard DATA limits 0/4096, including inside the stack band; exact zero peak; true one-byte BSS companion and insufficient-budget control. |
| Native BSS only extends left; anonymous mmap can merge right | `BssRightMerge` tracks surviving BSS across both images/reservations and refuses matching right contacts. | Native main/interpreter/cross-image two-VMA witnesses, exact host/runtime refusals and different-X full-parity companions. |
| ELF maps can replace tables that the loader already built | `InitialStackOverlap` excludes nonempty effective loads, whole first reservations and shadows from the source-derived top 8 MiB. | Native stack-overwrite main/interpreter witnesses, exact refusals, lower-band parity and admitted empty high loads. |
| The old upper bound includes a four-level reserved page | `ReservedTopPage` applies the conservative default-window bound to raw/effective ranges. | Native-valid high-page main/interpreter controls on this LA57 host, exact refusals and lower-band parity. |
| Final interpreter entry can enter loader-only low memory or escape the window | `InterpreterEntry` checks the resolved wrapping entry; C checks actual mmap-selected bias. | Fixed/empty-first high/low native outcomes, exact refusals, fixed companion and preserved negative-bias/main-entry parity. |
| Target inode changes exec personality/credentials/secureexec | `PrivilegedExecutable` refuses either setid bit or any `security.capability` xattr. | Own-ID 04755/02755 and verified empty-capability native starts, both refusals, same-inode restoration and full parity. |
| Dense/large maps permit a different low-hole/fallback allocator answer | `MappingTooLarge` caps each rounded load and attempted first reservation at 16 GiB; finite-header free-gap proof excludes that allocator path. | Main/interpreter extent and separate first-reservation controls at 16 GiB full parity and native-valid 16 GiB+PAGE exact refusals. |
| SYS_mmap huge-rounds hugetlb file length while ELF vm_mmap does not | `HugetlbElf` checks the pinned inode's filesystem before parsing/mapping. | Actual huge inodes, unfaulted 4 KiB request producing 2 MiB VMA, main/interpreter exact refusals and ordinary before/after parity. Native-valid huge ELF was not executed on this zero-pool host. |
| A boot guard override makes pre/post-table stack growth affect high allocations | `StackGuardGapOverride` refuses every visible override; default 1 MiB guard stays above the allocator ceiling. | Six strings through actual Rust and actual C guard/diagnostic: two accepted, four exact refusals. No boot setting is changed. |

The allocator argument uses the enforced 16 GiB request bound, at most 256 load
intervals plus two first reservations, and the minimum nonrandom mmap base
`0x155555556000`. Even after 4 TiB+32 GiB of obstacles, a gap exceeding 68.4 GiB
remains above 4 MiB. That exceeds any admitted request plus conservative
alignment/special-map slack; extra low loader VMAs cannot alter the highest
successful top-down answer or force fallback. The default 1 MiB guard plus
top 8 MiB initial-stack bound lies above the minimum 128 MiB mmap gap, so native
allocation before table growth and loader allocation afterward see the same
search ceiling. The detailed source arithmetic follows the mapping ledger.

All 14 new test functions and all 54 original tests passed in the final suite:
**68 = 1 unit + 45 parity + 22 preparation**, zero failed/ignored/filtered.
All seven original mutation controls executed. Required offline format,
clippy `-D warnings` and test gates passed on the unchanged manifest. See
[`REPORT.md`](REPORT.md), [`r5/REVIEW.md`](r5/REVIEW.md) and
[`r5/review.md`](r5/review.md) for the test receipts, independently checked
hashes and goalpost-moving review.

## Main ELF path and mmap policy computations

| Row | Kernel lines | Matching C lines | Matching R lines | Computation and disposition |
| --- | --- | --- | --- | --- |
| L01 | K:840–856,642–650; E:764–803,816–826 | C:234–243,517–539 | R:547–558,804–820 | Native requires an exec-authorized mmap-capable file; preparation/runtime pin readable regular files. Nonregular main/interpreter are explicit refusals. Caller owns exec authorization and immutable inode identity; reopening the O_PATH-pinned interpreter preserves the object. Hugetlb-backed objects are separately refused (L43). |
| L02 | K:842–850,946–950,643–647; x86 asm/elf.h:71–73,146–151 | C:253–263 | R:759–777 | ELF header selects native ELF64 x86-64 ET_EXEC/ET_DYN geometry. Rust/C additionally require class64, LSB, current versions, ehsize64 and phentsize56. ELF32/x32, other arches/types or inconsistent sizes have named UnsupportedElf/header refusals; unused section-header fields do not affect placement. |
| L03 | K:516–523 | C:261–268 | R:770–775,824–830 | Program-header byte count is e_phnum×56. Kernel permits nonzero counts up to floor(65536/56); loader conservatively caps at128. Multiplication cannot overflow for admitted counts. Zero/>128 are named header-count refusals. |
| L04 | K:529–530; elf_read K:465–474 | C:179–188,264–268 | R:779–788,821–830,1289–1292 | Read phdr bytes at e_phoff. C checks the complete interval against fstat size; Rust exact positional read refuses truncation/I/O and template parser checked-adds the end. Overflow/truncation is refused. File offset is not substituted with a virtual address. |
| L05 | K:867–876,878–884 | C:587–593 | R:855–860 | PT_INTERP p_filesz determines a NUL-inclusive filename extent of2..PATH_MAX. Matching read at p_offset. Missing interpreter is MissingInterpreter; repeated PT_INTERP and invalid length are named InvalidElf/runtime refusals instead of following the kernel's first-match selection. |
| L06 | K:887–916 | C:591–594,600–603 | R:861–867,547–558 | Kernel opens the first terminated PT_INTERP string. Loader requires exactly one trailing NUL and refuses embedded NUL bytes. For admitted strings, exact filename bytes and pinned regular object match. An interpreter's nested PT_INTERP is not recursively executed; the parser's stricter validation can refuse it. |
| L07 | K:923–931; E:653–665 | C:595–601; loader.ld:20 | R:1034–1042 | Main GNU_STACK PF_X selects EXSTACK_ENABLE_X versus DISABLE_X. Exactly one non-X header is admitted, so template and target both produce non-X stack. Missing/X/duplicate headers are ExecutableStack refusals. PF_R/PF_W, p_memsz, p_vaddr, p_offset and p_align do not size or place the x64 stack. Interpreter GNU_STACK has no influence. |
| L08 | K:540–623,933–939,959–990,713–815; include/linux/elf.h:88–109; arch/x86/Kconfig (no selection of ARCH_BINFMT_ELF_STATE, ARCH_HAVE_ELF_PROT or ARCH_USE_GNU_PROPERTY) | C:195–199,271–287 | R:712–722,835–868 | x86 uses dummy arch_elf_pt_proc/arch_check_elf and identity arch_elf_adjust_prot; ARCH_USE_GNU_PROPERTY is not selected here. Property parser returns at K:774–775 before note size/offset/8-byte-rounding computations. Ignoring processor/property notes is equivalent for admitted x64 source configuration; other architectures are refused. This ELF path does not activate CET from GNU properties. |
| L09 | K:995–1017; E:1181–1184,1196–1208,1534–1583; x86 process_64.c:717–734 | C:244–251,553–561 | R:541–542,1223–1260; R:18–22 | Under the caller contract, both execs clear TIF_ADDR32/READ_IMPLIES_EXEC and use identical caller personality, rlimits and resulting credentials/secureexec. Setid modes and any security.capability xattr on the main object get PrivilegedExecutable in Rust/C. Inode-sensitive LSM differences and binfmt_misc recursion must be excluded by caller authorization; this unwired loader does not implement those policy transitions. |
| L10 | K:820,1018–1023 | C:605–608,617 | R:1044–1046,1074–1094 | Bias begins0; data/memory extrema begin0; code minimum begins~0UL natively. Positive bounded main bias distributes over max operations. Rust/C retain target data extrema; native code extrema are recorded as an explicit existing mm residual (L19/L20/L27). |
| L11 | K:1034–1042,1051–1058,1181–1185 | C:274–276,605–610,707–711 | R:850–853,1044–1047 | First PT_LOAD in program-header order decides ET_EXEC versus PIE placement and raw first vaddr. Ascending addresses are required; unordered loads receive named UnsupportedElf/runtime refusal instead of alternate order semantics. Empty loads still consume first-load state. |
| L12 | K:476–494,1084–1092,1123–1131,1170; asm/elf.h:235–236 | C:300–307,610–615 | R:1051–1059; R:51 | PIE bias: PAGEALIGN(max power-of-two p_align from0); base=(DEFAULT_MAP_WINDOW/3)*2; conditional nonzero-alignment mask; PAGESTART(base−first raw vaddr). Fixed all-zero alignment keeps base0x555555554aaa before subtraction. Native negative main bias is conservatively refused; negative interpreter bias remains admitted. See M05–M10. |
| L13 | K:1132–1161,1306–1312 | C:600 | R:1034 | Static ET_DYN has an allocator-selected/aligned first map and possibly relocated brk. All mains without PT_INTERP are MissingInterpreter/runtime refusals, so those address computations are outside admitted input. Interpreter ET_DYN follows load_elf_interp, not this static-main branch. |
| L14 | K:1040,1049–1057,1084–1089,1131,1181–1187 | C:697–711 | R:1061–1072,1124–1134 | First main file load uses PRIVATE+FIXED_NOREPLACE; later ones use FIXED. Only first PIE gets total span. A zero-filesz first load makes no reservation but consumes first state; fixed successful first-map bias adjustment is0. Attempted reservations above16GiB are MappingTooLarge; actual stack intersection is InitialStackOverlap. |
| L15 | K:1173–1178 | C:620–623,703–706,368–376 | R:883–895,1293–1339 | Effective request is main_bias+p_vaddr, then file mapping rounds start down. Rust uses checked addition; C validates resulting full ranges before replacing mappings or zeroing. Bounded admitted geometry is equivalent. Low/upper/stack/oversized ranges have named refusals. |
| L16 | K:355–392,399–445 | C:344–407 | R:1124–1194,1293–1339 | Rounded file size, file offset, first reservation, tail release and BSS sizes/protections are covered individually by M15–M31. SYS_mmap uses the same ordinary backing/offset/prot/flags; hugetlb's syscall-only rounding is refused. All nonempty mapped extents and attempted reservations are capped, including unaligned offset bytes. |
| L17 | K:104,1207–1216,692–698; asm/page_64.h:83–95; page_64_types.h:54,62–70 | C:206–232,278–286 | R:840–848,897–906,1293–1319 | Kernel raw/effective TASK_SIZE checks are u64 bounds with filesz≤memsz. Loader additionally admits only below0x7ffffffff000 and below its complete-page bound. ReservedTopPage closes the four-level final-page error; this is conservative on this LA57 host, actual TASK_SIZE0xfffffffffff000. Native high-page control succeeds here before both named refusals. |
| L18 | K:1194–1198,1231 | C:712–716,775 | R:1102–1113,632–634 | AT_PHDR uses the last PT_LOAD for which offset≤e_phoff<offset+filesz, then vaddr+(phoff−offset)+bias. Same start-only predicate as kernel. No containing load is a named UnsupportedElf refusal; the complete table need not be artificially contained in one segment. Duplicate-header controls retain exact comparisons. |
| L19 | K:1200–1202,1233,1288 | Kernel template exec; C:683–691 removes shadow without changing code fields | R:445–505 | Native start_code=min raw vaddr of PF_X loads, then+bias (including sentinel behavior without PF_X). Loader mm code minimum names its own executable. Existing exact procfs residual stat26 is disclosed; target mapping addresses/protections still match. No false code-field equivalence claim. |
| L20 | K:1219–1222,1234,1287 | Kernel template exec; C:683–691 removes shadow without changing code fields | R:445–505 | Native end_code=max(vaddr+filesz) over PF_X, then+bias. Loader mm code maximum remains loader code. Existing exact residual stat27 is retained and tested as part of the unwired mm identity surface. |
| L21 | K:1203–1204,1235,1289 | C:617–624,683 | R:1074,483–503 | start_data=max all PT_LOAD vaddr+bias, independent of PF_W and including empty loads. Sparse shadow sets that same vaddr; kernel sets the native mm field. Equivalent under bounded positive main bias. |
| L22 | K:1219,1223–1224,1236,1290 | C:625,684 | R:1075–1084,462–466 | end_data=max(vaddr+filesz)+bias. Shadow filesz=end_data−start_data. Checked host arithmetic refuses overflow/negative difference. Same raw unaligned native value, not page rounding. |
| L23 | K:1225–1227,1232 | C:626,685 | R:1085–1094,467–471 | memory_end=max(vaddr+memsz)+bias is unrounded, including empty last loads. New public field preserves it independently from rounded brk. Shadow memsz=memory_end−start_data fixes the extra-page defect. |
| L24 | K:1230,244 | C:777 | R:1109,644–646 | AT_ENTRY=raw main e_entry+main bias. Unlike final interpreter transfer, kernel does not require main AT_ENTRY to belong to a PT_LOAD or pass BAD_ADDR when an interpreter exists. Loader likewise imposes no invented membership/range check; Rust refuses only arithmetic overflow. |
| L25 | K:630–706,1239–1248 | C:718–747 | R:909–957,636–642 | load_elf_interp returns relocation adjustment (zero/negative/allocator-selected), not entry. First-file hint/no_base, first-empty state, total-span consumption and ET_EXEC fixed overlap are covered by M32–M36. Interpreter PT_LOAD p_align does not introduce maximum_alignment masking. |
| L26 | K:1248–1254 | C:638–650,750–754,788 | R:959–982 | Transfer entry is wrapping interp_bias+e_entry followed by range validation. InterpreterEntry refuses resolved low/out-of-window entry; unknown first-file bias is checked after actual maps, raw high entries conservatively refused. One-byte positive-span controls may legitimately enter main memory, including negative-bias cases. |
| L27 | K:1287–1290 | C:683–691 | R:462–503,1074–1118 | Kernel template exec installs exact native data fields through shadow, which is later removed. start_data/end_data match every admitted layout; code fields26/27 retain explicitly allowed loader values. /proc/self/exe likewise remains loader inode identity. These residuals cannot be presented as complete mm equality. |
| L28 | K:1291; E:713; K:287–311 | C:562–577,787; entry.S:13–27,146 | R:11–22,534–535,667–697 | start_stack/final RSP is the kernel's final table pointer, kept unchanged. Identical argv/envp and equal execfn extent produce the same pointer under the caller/host contract. Loader never pushes onto original stack; separate shared scratch permits tiny RLIMIT_STACK without loader-induced stack growth. Nonempty ELF/shadow/table overwrite is refused. |
| L29 | K:1306–1329 | C:628,688; C:554–556 | R:1098,475–476,1034 | mm->start_brk=mm->brk=PAGEALIGN(memory_end). Static-main relocation is refused; PF_RANDOMIZE is off, so random brk gap, arch_randomize_brk and brk_randomized changes are absent. C verifies existing kernel brk rather than trying to repair it with brk(2). |
| L30 | K:406–443,1289–1290,1313 (elf_load via ordinary template exec) | C:683–688 | R:454–503 | Sparse shadow: offset=PAGESTART(template_len+0x10000)+POFF(start_data), flagsR, vaddr/paddr=start_data, filesz=end_data−start_data, memsz=raw memory_end−start_data, alignPAGE. Native kernel alone rounds brk. Public layout write repeats low/top/stack/size and rounded-brk consistency checks before any output mutation. |
| L31 | K:406–443,1313 | C:689–691,190–193 | R:469–503,1098–1101 | Remove only actual shadow extent: no unmap for memsz0; otherwise PAGEALIGN(POFF(vaddr)+memsz) at PAGESTART(vaddr). Empty unaligned high headers map no page, even inside stack band. Exact hardDATA0/4096 controls plus true one-byte BSS verify allocation and brk separately. |
| L32 | K:439–440; mm/mmap.c:376–378,1324–1343; mm/vma.h:412–418; mm/vma.c:2722–2724,2770–2781; include/linux/mman.h:201–213 | C:344–364,390–404; entry.S:15–26 | R:535–540,586–597,1136–1194 | Private writable file/reservation and anonymous RW(+X) BSS count as data_vm. Shared scratch excludes private data accounting. Finite AS and DATA below conservative footprint are named refusals. Executable anonymous BSS under inherited MDWE is MdweExecutableBss; same-page tail and non-X cases remain supported. Kernel map-count and resource charges also apply to loader syscalls. Rust does not query/refuse vm.max_map_count; retained/transient loader VMAs can cause extra failures under tiny host limits. Successful geometry requires resource headroom; complete failure/rollback equivalence is outside the existing README:62–63 backend protocol. |
| L33 | K:439; mm/vma.c:2785–2792,1062 versus mmap path2660–2664 | C:309–340,362–364,399–404 | R:566–584,984–1031,1196–1221 | vm_brk_flags only extends predecessor; anonymous mmap can also merge right. Track prior anonymous BSS, subtract full file reservations and replacements, preserve fragments, test surviving same-X right endpoint before new BSS. BssRightMerge refuses differences across both images. ≤256 loads add at most2 intervals each; C capacity512 is sufficient. Main/interpreter/cross-image witnesses qualify native two-VMA shape and different-X parity companions. |
| L34 | E:410–438,635–648,698–714; K:133–136,177,186–214,284–345; include/uapi/linux/resource.h:66 | C:27–29,206–232,683–691 | R:59–65,667–697,1068–1072,1293–1339 | Initial argv/envp strings plus reserved pointers≤6MiB; kernel adds≤128KiB expansion, execfn≤PATH_MAX, small platform/random/auxv bytes and page/16-byte rounding. Top8MiB covers any admitted initial argv/envp/RLIMIT_STACK. InitialStackOverlap excludes every nonempty load, entire attempted first reservation and shadow from that band. Empty headers remain accepted. |
| L35 | x86 mm/mmap.c:62–68,111–143; E:1206–1208 | C:554–556,355,757 | R:18–22,541–542,1251–1260 | ADDR_COMPAT_LAYOUT/sysctl choose legacy versus top-down mm flags/base. Both template and native exec use same post-secureexec stack snapshot and personality. Loader does not replace allocator policy with a guessed interpreter base; syscall uses the inherited mm allocator. |
| L36 | x86 mm/mmap.c:70–79,41–49; K:1126–1127 | C:554–556,612–615 | R:18–22,1051–1059 | arch_rnd and stack_maxrandom_size return0 when PF_RANDOMIZE is clear. Native mask/shift/get_random_long branches have no address contribution. ASLR-on is an explicit runtime refusal; intrinsically random AT_RANDOM bytes remain separate. |
| L37 | mm/mmap.c:941–954; include/linux/mm.h:3435–3455; mm/vma.c:2921–2930 | C:156–177,557,792–801 | R:542,1251–1260,1353–1383 | stack_guard_gap defaults to256 pages=1MiB; boot parser may assign val<<12. Large overrides expose native-before-tables versus loader-after-tables allocation ceilings, including successful high-address mismatches. StackGuardGapOverride conservatively refuses every visible occurrence, even small/invalid values. Actual Rust/C guards are tested on six strings without boot changes. Default1MiB+top8MiB<minimum128MiB mmap gap closes this ordering difference. |
| L38 | x86 mm/mmap.c:82–99,111–131 | C:58–62,554–557 | R:59–70,542,1251–1260 | mmap gap starts rlimit_stack; add stack random maximum(0)+guard with overflow check; clamp to[128MiB,5*floor(DEFAULT_MAP_WINDOW/6)]; base=PAGEALIGN(window−gap). Both execs delegate same formula/rlimit. Minimum base is0x155555556000; maximum is window−128MiB. Default guard cannot intersect allocator ceiling for bounded initial stack. |
| L39 | x86 mm/mmap.c:101–104,146–157; asm/processor.h:677–678; sys_x86_64.c:92–116 | C:355,757; C:52–62 | R:70,909–957,1063–1067,1321–1330 | Legacy base=PAGEALIGN(DEFAULT_MAP_WINDOW/3)=0x2aaaaaaab000, random addition0. Nonfixed normal64-bit search end=DEFAULT_MAP_WINDOW unless hint above it. All admitted raw hints are below that window; MAP_32BIT is never supplied. Retained low mappings lie below legacy search beginning. |
| L40 | sys_x86_64.c:29–55,85–89,143–160,188–197 | C:345–355,733–741 | R:909–957 | Nonfixed hint uses PAGESTART for top-down, PAGEALIGN in bottom-up after C/kernel rounded original vaddr to a page. File offset passes in page units to same kernel callback. Ordinary x86 cache-color align mask/bits are0 with ASLR off. Preserved low file hints are InterpreterHintOverlapsLoader; discarded/zero hints and empty-first biases retain exact controls. |
| L41 | sys_x86_64.c:200–229; x86 mm/mmap.c:36–38 | C:206–232,355,757 | R:897–906,909–957,1293–1330 | Top-down uses low PAGE_SIZE and high mm mmap_base; full-LA57 extension occurs only for hint>DEFAULT_MAP_WINDOW, excluded by scope. Stack-gap treatment sees identical ceiling under default-guard admission. Target/interpreter ordinary mapping intervals match, with only disclosed low loader VMAs added. |
| L42 | sys_x86_64.c:232–239; mm/vma.c:2898–2938; x86 mm/mmap.c:82–104 | C:52–62,201–232,350–359 | R:70,916–917,1063–1067,1315,1321–1330 | Dense native maps could formerly force native low-hole selection and a different loader bottom-up fallback. MappingTooLarge caps each rounded extent/attempted reservation at16GiB. ≤258 intervals occupy≤4TiB+32GiB; at least one >68.4066GiB gap remains in[4MiB,min_base), far above16GiB request+alignment/special slack. The allocator therefore succeeds above low extras in both paths. Exact per-load and first-reservation boundary controls qualify the named scope restriction. |
| L43 | mm/mmap.c:566–583,827–852; fs/hugetlbfs/inode.c:172–189; arch/x86/mm/hugetlbpage.c:24–31; mm/shmem.c:2848,2887–2916; mm/huge_memory.c:1090–1147 | C:239–243,355 | R:809–820 | SYS_mmap huge-rounds hugetlb file length before the callback; native elf_map vm_mmap does not. HugetlbElf refuses the pinned inode's HUGETLBFS_MAGIC in both implementations. Ordinary file callbacks see identical file, request, flags and offset; shmem/THP inflation≤2MiB fits free-gap slack. Mandatory real-inode, unfaulted4KiB→2MiB VMA and exact rejection controls execute. Native-valid huge ELF needs unavailable pool pages; no such execution is claimed. |
| L44 | K:1275–1282; x86 vdso/vma.c:147–154,236–243; mm/mmap.c:842–852 | C:653–667,756–771 | R:541–565,647–650,70 | vDSO asks get_unmapped_area(NULL,0,total_special_size,0,0) after images. C asks ordinary anonymous private PROT_NONE with same extent after dropping old specials/shadow and loading both images. Span≤0x80000<PMD prevents syscall THP alignment branch. Default guard and cap close table-order and low-extra allocator differences; retained kernel special image is then relocated. |
| L45 | x86 vdso/vma.c:147–194,80–88; lib/vdso/datastore.c:99–104; mm/mremap.c:1039–1044,1812–1820 | C:428–490,653–667,763–771 | R:647–650; R:11–22 | Special base/text/vvar/vclock sizes/protections are covered by V01–V10. Relative offset=old_component_low−old_union_low; final=slot+offset. Full-VMA mremap preserves kernel objects, special protections and mm vdso context. Missing/oversized/malformed/sealed special layouts have existing named runtime refusals. Fixed mremap additionally requires spare map-count slots and allocation resources; loader-only failure here is diagnosed at C:485–488 rather than refused by Rust preparation. This row does not claim full platform or failure-availability equality. |
| L46 | K:148–349; asm/elf.h:331–337 | C:562–581,768–782 | R:534–535,631–655,667–697 | create_elf_tables computation rowsA01–A37 enumerate every allocation, scalar and pointer. Identical argv/envp and equal execfn extent produce the same RSP, pointers and ordering under the caller/host contract; C changes only planned fixed-size values and equal-length execfn bytes. Saved procfs auxv is untouched. Stack scalar patch sites are {3,5,7,9,33}; actual procfs differences can be smaller when values coincide and retain fixture-exact sets. Stack auxv is exact after AT_RANDOM witness normalization. |
| L47 | K:1358–1362; asm/elf.h:153–167; x86 process_64.c:531–587 | C:787–789; entry.S:35–76,112–124,129–147 | R:11–22,445–451,631–655 | START_THREAD transfer RIP=resolved interpreter entry, RSP=table p; ELF_PLAT_INIT is x64 register zeroing, no function-descriptor relocation. Assembly resets native observed state while preserving mapping-created PKRU, then jumps using record without leaking GPRs. This audit adds no register-comparator exemptions; prior round4 xstate evidence remains applicable. |
| L48 | K:1332–1343; E:1184 | C:206–232,554–557 | R:18–22,883–906 | Preserved MMAP_PAGE_ZERO causes kernel to map PAGE0 R+X and attempt sealing in both native/template exec. Every nonempty target/interpreter mapping below4MiB is refused; empty interpreter headers map nothing. Mapping order cannot change page0 contents. The loader does not unmap or emulate page0; ADDR_LIMIT_32BIT does not switch x64 TIF_ADDR32. |

## ELF mapping, alignment and BSS helper computations

| Row | Kernel lines | Matching C lines | Matching R lines | Computation and disposition |
| --- | --- | --- | --- | --- |
| M01 | K:81–93; x86 `asm/elf.h:228` | C:23,30–32 | R:50,1341–1346 | x86 ELF_MIN_ALIGN/PAGE is 4096; start `v & -PAGE`, offset `v & (PAGE-1)`, end `(v+PAGE-1)&-PAGE`. Equivalent on bounded admitted values; overflow/raw geometry is refused by R:837–848,1289–1292 and C:278–286. |
| M02 | K:457 | C:294 | R:1130 | Span minimum is minimum **page-rounded** PT_LOAD vaddr, including empty loads. Equivalent. Empty-first controls cover the resulting bias. |
| M03 | K:458 | C:295–296 | R:1131 | Span maximum is max `vaddr+memsz`, without rounding. Equivalent. Zero and one-byte-span controls distinguish rounding. |
| M04 | K:452,459,462 | C:292–298 | R:870–871,1133 | Span is max minus min if any PT_LOAD exists; kernel returns zero without loads. No-load inputs have named `InvalidElf("no PT_LOAD")` refusal rather than an equivalence claim. |
| M05 | K:478–489 | C:300–305 | R:846–848,1051 | Maximum alignment begins zero and considers only power-of-two PT_LOAD alignments; zero is not a power of two. FIXED: both implementations retain zero. Non-power-of-two alignments above one are explicitly refused instead of ignored; all-zero and mixed-alignment native controls qualify the fix. |
| M06 | K:492–493 | C:306 | R:1051,1344–1346 | PAGEALIGN(max_alignment) leaves zero zero and rounds 1..PAGE to PAGE. FIXED and equivalent; all-zero and mixed zero/one/large alignment controls discriminate the conditional mask. |
| M07 | x86 `asm/elf.h:235–236`; K:1125 | C:33,612 | R:51,1053 | PIE base is `(DEFAULT_MAP_WINDOW/3)*2 = 0x555555554aaa`. Equivalent for x86-64. Other ELF architectures/classes are refused. |
| M08 | K:1126–1127 | C:554–556 | R:18–22 ASLR-off invocation contract | PIE random addition executes only with PF_RANDOMIZE. ASLR-off is a required invocation precondition and C checks ADDR_NO_RANDOMIZE; zero addition is equivalent. |
| M09 | K:1129–1130 | C:611–613 | R:1051–1056 | Mask base with `~(alignment-1)` only if alignment is nonzero. FIXED and equivalent, including first vaddr/offset `0x123` controls. |
| M10 | K:1170 | C:614–615 | R:1057–1059 | Bias is PAGESTART(masked base minus first raw vaddr). Equivalent where first vaddr ≤ base; negative main bias receives `UnsupportedElf("PIE first segment exceeds native load base")` refusal. Negative interpreter bias remains admitted. |
| M11 | K:1049–1057,1131 | C:702–705 | R:1061–1073 / no syscall generation | First ET_EXEC/PIE file load uses PRIVATE\|FIXED_NOREPLACE; later loads use PRIVATE\|FIXED. Equivalent; the first load with no file bytes deliberately performs no file-map reservation. |
| M12 | K:1084–1089 | C:697,705 | R:1102–1105,1124–1134 | Only the first PIE main load uses the whole-image span; ET_EXEC uses no span. A zero main PIE span cannot pass mapped-PHDR admission at R:1102–1105. Zero interpreter span has its separate named refusal. Attempted nonempty first reservations over 16 GiB receive MappingTooLarge; empty first loads consume total without any reservation and remain supported. |
| M13 | K:1181–1185 | C:707–710 | R:1108 | After first main map, add `map_result-PAGESTART(old_bias+first_vaddr)` to PIE bias. Fixed NOREPLACE returns the request on admitted successful mappings, so the adjustment is zero. Empty first loads return their rounded request, also zero. |
| M14 | K:1173–1174 | C:703–705 | R:892,954,993 | Segment request is `bias+p_vaddr`, with the same per-segment prot and mapping flags. Equivalent once bounded effective geometry is admitted. |
| M15 | K:360,363 | C:346 | R:837–848,1167 | File length is PAGEALIGN(`p_filesz+PAGEOFFSET(p_vaddr)`). Equivalent; byte/file/memory overflow and filesz>memsz are refused. |
| M16 | K:361 | C:347 | R:837,846–848 | File offset is `p_offset-PAGEOFFSET(p_vaddr)`. Page-congruence admission guarantees no underflow. Noncongruent input receives PT_LOAD alignment refusal. |
| M17 | K:362 | C:348 | R:1293–1320,1341–1343 | File-map address is PAGESTART(request). Equivalent for admitted addresses. |
| M18 | K:367–368; K:406 | C:349,374,382–385 | R:1164–1167 | Zero computed map length returns address without mmap; elf_load skips elf_map when filesz is zero, even at an unaligned vaddr. Equivalent, including empty-first interpreter controls. |
| M19 | K:378–384; mm/mmap.c:566–580; hugetlbfs/inode.c:180–181 | C:239–243,350–355 | R:809–820,1062–1073,1170–1173 | First file mmap length is PAGEALIGN(total_span), otherwise the rounded file length. Equivalent for admitted non-hugetlb files; oversized attempts receive MappingTooLarge. Native vm_mmap does not perform syscall mmap's prior huge-page rounding, so hugetlb-backed main/interpreter files instead receive named HugetlbElf refusals. hugetlb_inode_controls proves real inode identification, a 4 KiB syscall producing a 2 MiB VMA without faulting it, host/defensive refusals, and ordinary full-parity companions. C's length<file-size refusal is unreachable after validated memsz≥filesz. |
| M20 | K:381–382 | C:356–364 | R:995–1000 / span accounting only | After a successful whole-span map, release `[mapped+file_size,mapped+rounded_span)`. Equivalent. Zero-length native vm_munmap can fail harmlessly; C avoids issuing that no-effect call. |
| M21 | K:610–622; `include/linux/elf.h:104–108`; x86 process_64.c:717–733 | C:195–198 | R:1169–1189 | Prot bits come from PF_R/W/X; x86 has identity arch_elf_adjust_prot. 64-bit exec clears inherited READ_IMPLIES_EXEC for both native and loader image, so actual permissions match. Execute-only PKRU controls cover the kernel mmap side effects. |
| M22 | K:411–412 | C:378 | R:1146,1185 | File-backed zero start is `mapped+PAGEOFFSET(vaddr)+filesz`. Equivalent. |
| M23 | K:413–414 | C:379 | R:1148,1187 | File-backed zero end is `mapped+PAGEOFFSET(vaddr)+memsz`. Equivalent. |
| M24 | K:115–119,420–421 | C:380–381 | R:1136–1157 | Padzero length is zero on an aligned end, otherwise `PAGE-PAGEOFFSET(zero_start)`. Native attempts the write and ignores an error for a nonwritable segment; C writes only PF_W segments. On x86 the nonwritable VMA rejects the first write, making the bytes unchanged in both cases. Writable mappings execute the same clearing. |
| M25 | K:423–426 | C:382–385 | R:1143–1148,1182–1187 | Pure BSS maps no file page; map_result/zero_start is PAGESTART(request); zero_end includes original raw vaddr offset plus memsz. Equivalent, including negative empty-first bias. |
| M26 | K:428,436–437 | C:387–390 | R:1143–1149,1182–1188 | Anonymous BSS begins at rounded zero_start and ends at rounded zero_end, only when memsz>filesz; zero length maps no pages. Equivalent. |
| M27 | K:439–440; `mm/vma.c:2770`; x86 page_types.h:29; mm.h:429–430 | C:400–402 | R:1178–1188 | BSS is always anonymous RW and gains X iff file prot had X, even for a read-only original load. Same address, size, permissions and private data accounting. MDWE exception is separately refused. |
| M28 | K:439; `mm/mmap.c:1229–1238` | C:400–404 | R:1006–1027 | Native vm_brk_flags uses MAP_FIXED range validation and unmaps overlap before creating BSS; C uses anonymous PRIVATE\|FIXED mmap. Both replace overlap, including BSS following a first NOREPLACE file segment. No blanket NOREPLACE protection applies to BSS in native exec. |
| M29 | K:439; `mm/vma.c:2785–2792,1062` versus mmap path:2660–2664 | C:309–340,362,397–404 | R:566–584,984–1031,1196–1221 | NAMED REFUSAL `BssRightMerge`: kernel considers only the previous VMA for anonymous BSS extension; anonymous mmap also considers the next VMA. Both implementations track replacement of prior BSS, including complete first reservations and their released tails, and refuse surviving right BSS of the same execute protection. Host controls cover decidable main/fixed/empty-first interpreter bias; C checks actual cross-image addresses even when the interpreter base comes from mmap. `kernel_bss_right_merge_controls` retains the baseline failure evidence and adds main, interpreter, cross-image native/refusal/defensive controls plus exact-parity companions. |
| M30 | K:439–440; `mm/mmap.c:1329–1343`; `mm/vma.c:2770–2781`; `mm/vma.h:415–417` | C:400–402 | R:536–539,590–598,1159–1193 | Private writable file/BSS bytes count toward RLIMIT_DATA, including RWX BSS. Finite RLIMIT_AS is refused because of loader overhead; insufficient/conservatively overlapping data budgets receive FiniteDataLimitBelowStartupFootprint. Qualifying startup boundary and brk-growth controls retain exact requests/results. |
| M31 | K:439–440 | C:391–395 | R:586–588,1136–1157 | Under inherited MDWE the native vm_brk_flags executable-BSS special case cannot be reproduced by mmap. Named MdweExecutableBss refusal with native-valid MDWE-on and exact MDWE-off/same-page companions. |
| M32 | K:652–657 | C:719–722 | R:560–563,1124–1134 | Interpreter span uses the same helper and zero span is refused. Existing zero/positive controls prove native SIGSEGV vs named preparation/loader diagnostics and one-byte positive parity. |
| M33 | K:636–637,668–672 | C:718,727–739 | R:909–957 | Interpreter begins with zero bias. ET_DYN first raw vaddr remains a nonfixed hint when main bias is zero; biased main replaces it with `-vaddr`, making first request zero. Equivalent except preserved nonzero low hints are named InterpreterHintOverlapsLoader refusals. |
| M34 | K:669–670 | C:728–732 | R:919–926 | ET_EXEC interpreter uses PRIVATE\|FIXED for every file map, including first, so it can legitimately replace main mappings. ET_DYN uses nonfixed first then fixed later. C setting interp_set for ET_EXEC is harmless because ET_EXEC independently selects fixed every iteration. |
| M35 | K:674–676 | C:741–743 | R:916–925,929–941 | Only first interpreter file-map attempt gets total span; first empty load consumes it without a reservation. Equivalent, with empty/empty-BSS controls proving the distinction. |
| M36 | K:681–684 | C:745–747 | R:929–934,972–977 | First ET_DYN interpreter bias becomes `map_result-PAGESTART(first_vaddr)`. Empty first returns PAGESTART(request); thus bias is zero under an unbiased main or `-PAGESTART(first_vaddr)` under a biased main. Wrapping subtraction is intentional and supported. |
| M37 | K:692–698; K:1207–1216 | C:206–232,278–286,372–377 | R:840–848,883–907,909–957,1293–1329 | Kernel checks TASK_SIZE and filesz≤memsz; loader additionally checks rounded effective extents and its low band. Named LowLoadSegment and UnsupportedElf refusals qualify raw/biased boundaries. ReservedTopPage restricts scope to the four-level MAP_LIMIT despite the current LA57 host. MappingTooLarge caps all nonempty extents and attempted reservations at 16 GiB; kernel_mapping_size_controls verifies exact native-valid main/interpreter 16 GiB+PAGE extents and a distinct first reservation, exact host/runtime refusals, and full-parity 16 GiB companions. The cap and default-stack-guard restriction prove retained low mappings cannot change a successful allocator result; see appendix. |
| M38 | K:1194–1198,1231 | C:712–716,775 | R:1102–1113,632–634 | Select the **last** PT_LOAD containing e_phoff by the kernel's start-only predicate, and compute `e_phoff-p_offset+p_vaddr+bias`. Equivalent. No mapped e_phoff is refused by name; duplicate-header control fixes the former first-match discrepancy. |
| M39 | K:1203–1204,1235 | C:624 | R:1074 | start_data is max raw PT_LOAD vaddr plus bias. C takes max after positive main bias; distributivity is valid on admitted bounded addresses. Placeholder sets the native mm field. |
| M40 | K:1219,1223–1224,1236 | C:625 | R:1075–1084 | end_data is max raw `vaddr+filesz` plus bias. Equivalent; arithmetic overflow is refused. |
| M41 | K:1225–1227,1232 | C:626 | R:1085–1094 | elf_brk is max raw `vaddr+memsz` plus bias, including empty loads. Equivalent. |
| M42 | K:1313; elf_load406–443 | C:628,683–691 | R:462–497,1085–1101 | Final mm start_brk/brk is PAGEALIGN(elf_brk). FIXED: sparse shadow carries vaddr=start_data, filesz=end_data-start_data and memsz=unrounded memory_end-start_data. The kernel itself rounds brk. C verifies raw memsz and unmaps only actual nonempty shadow mappings. Empty highest unaligned load controls below and inside the stack band require full entries/observer parity, emitted filesz=memsz=0, no map on the empty page, exact metadata, and exact peak VmData. |
| M43 | K:1306–1312,1315–1329 | C:600,554–556,628 | R:1034,1046–1073 | Static ET_DYN relocation of brk applies only without PT_INTERP, which is refused. Random brk gap/addition is off with required ASLR-off. Equivalent for admitted dynamic mains. |
| M44 | K:1200–1202,1221–1222,1233–1234,1287–1288 | C:553–790 loader_main retains its kernel-set mm code fields; no writer | R:457–497 sparse-shadow design | Native start_code/end_code use executable-load extrema; loader kernel mm code fields deliberately name loader code. Existing comparison asserts exactly stat fields 26/27 differ, rather than claiming equality. This is an already documented unwired-loader residual, not a new geometry fix. |
| M45 | K:1230 | C:777 | R:1109,644–647 | AT_ENTRY is main raw e_entry plus main bias. C's unsigned addition matches kernel; R refuses overflow when constructing its plan. Empty interpreter controls deliberately permit entry in main memory. |
| M46 | K:1243–1254 | C:638–651,750–754,788 | R:565,959–982 | Interpreter transfer entry is wrapping `interp_bias+interp.e_entry`, checked by kernel after addition. NAMED REFUSAL `InterpreterEntry` covers a resolved entry below 0x400000 or at/above scoped MAP_LIMIT=0x7ffffffff000. Preparation handles known zero/negative biases, conservatively refuses raw upper entries for allocator-selected bias, and C validates the final biased value after mapping. `kernel_interpreter_entry_controls` adds exact native outcomes, host errors, defensive loader diagnostics, and an exact fixed-interpreter companion. Existing supported negative-bias/main-entry cases are retained. |
| M47 | K:1013–1014,1173,1275–1282; mm/mmap.c:941–954; x86/mm/mmap.c:82–98; mm/vma.c:2921–2926 | C:156–178,557,206–232,353–359,396,683–691 | R:542,487,919–925,1062–1073,1101,1251–1260,1293–1340 | Native creates initial tables after main/interpreter/vDSO mapping; loader reuses completed tables. InitialStackOverlap refuses nonempty mappings/reservations/shadows intersecting the protected 8 MiB band; empty loads with empty shadows remain exact-parity controls. StackGuardGapOverride also refuses every visible boot override: a giant guard could make native pre-table and loader post-table stack VMA starts produce different high allocator ceilings. Mandatory Rust/C guard controls use default, huge, small, quoted, and invalid boot strings. This host restriction resolves that case; no insufficient fallback-only case split remains. |
| M48 | K: former helper name in stale comment1214; live elf_load439 and brk1313 | C:368–407,628 | R:1033–1122,1159–1193 | Linux 6.17 has **no live set_brk helper**. Its responsibilities are divided between per-segment vm_brk_flags and final mm brk assignment; rows M25–31 and M41–43 audit those live computations directly. |

## Allocator gap and table-order proof

The live x86 allocator is `arch/x86/kernel/sys_x86_64.c:127–239`, selected by
`mm/mmap.c:800–808`. With ASLR off, `get_align_mask` returns zero for normal
file mappings (`sys_x86_64.c:29–40`). A valid preserved nonzero hint is tested
against the next VMA first (`:188–196`); low preserved file hints are explicitly
refused. Otherwise top-down searches `[PAGE, mmap_base)` (`:200–227`). Failure
calls the bottom-up helper (`:232–239`), whose `find_start_end` uses
`mmap_legacy_base`, not the failed top-down base (`:92–115,138–163`).

For `T=DEFAULT_MAP_WINDOW=0x7ffffffff000`, the mmap gap calculation in
`arch/x86/mm/mmap.c:82–105` yields:

```
minimum_topdown_base = PAGEALIGN(T - 5*floor(T/6)) = 0x155555556000
legacy_base         = PAGEALIGN(T/3)             = 0x2aaaaaaab000
L                   = 16 GiB
N                   = 2*128 + 2 = 258
window_bytes        = minimum_topdown_base - 4 MiB = 23,456,243,867,648
occupied_bytes      <= N*L                         =  4,432,406,249,472
largest_free_gap    >= floor((window_bytes-N*L)/(N+1))
                    = 73,451,110,494 bytes > 68.4 GiB
```

There are at most 256 PT_LOAD extents plus two first reservations. Every actual
nonempty extent/reservation is capped at L; entirely empty headers map nothing.
Counting both reservations is conservative even after their tails are released.
The free gap is larger than one L-sized request, 1 GiB file-alignment allowance,
and the bounded special mapping span. C:59–62 encodes a stricter inequality;
C:201–218,350–359 and R:916–917,1062–1073,1315–1329 enforce its size premise.
Consequently native top-down has a viable range above the loader band. All
mappings there are identical before each allocation, so additional low loader
mappings cannot change its highest answer or cause loader-only fallback. Legacy
allocation starts above that band and likewise cannot inspect the extra low
VMAs. Hugetlb inputs are separately refused because syscall/native lengths differ.

The proof also needs the same effective stack ceiling. `setup_arg_pages` adds
128 KiB expansion, while later pointer tables can exceed that expansion; native
calls its allocators before those tables are built. Maple-tree top-down retry
uses `high_limit=vm_start_gap(next_stack)` (`mm/vma.c:2921–2926`), so a giant boot
guard can expose the different stack VMA starts even without any low fallback.
That discovered case is resolved by the explicit `StackGuardGapOverride`
host refusal, rather than an insufficient fallback-only case split. With the
default 1 MiB guard (`mm/mmap.c:941`) and the admitted 8 MiB stack bound, the guard
is at least roughly 119 MiB above the highest possible mmap ceiling: the mmap
gap is at least 128 MiB (`arch/x86/mm/mmap.c:96–98`). Table construction therefore
cannot change the allocator's search ceiling on admitted hosts. C:156–178,557
and R:542,1251–1260 enforce the host premise; the guard control invokes both
actual implementations without changing a boot setting.

`kernel_mapping_size_controls` independently qualifies L-sized main/interpreter
BSS extents with full parity, then starts native L+PAGE controls and requires
exact host/error fields and defensive diagnostics. A third subcontrol uses an
L-sized versus L+PAGE interpreter first reservation backed by the same tiny
read-only file: native releases the high empty page and reaches the observer;
only the loader refusal changes. That companion/control has 276 KiB peak VmData,
so it does not commit the large virtual span. No TB-size mapping probe ran.
Evidence: `kernel-mapping-size-with-reservation.log`.

## Validation and goalpost assessment

The direct defect probe used the ordinary unchanged comparison code. Its
mapping failure is retained as a failure, with the complete native and loader
maps, and is not relabelled as parity. Existing assertions, tolerances,
comparators and mutation checks have not been edited by this auditor.
New refusals have native controls, exact enum fields, exact exit/stderr
diagnostics with preparation bypassed, and qualifying companions that use the
ordinary full entries/observer comparison. They are named loader scope
restrictions, not claims that those successful native ELFs are invalid.

The earlier focused log (`kernel-controls-focused.log`) retains its one failure:
the old shadow still rejected a high empty load with a geometry mismatch. The
actual shadow geometry was then fixed. Its new controls require stronger exact
parity and independent empty-shadow/metadata/VmData assertions; the old mismatch
was not renamed as a pass. The LA57 top-page controls assert exact native entry
on a host where the independent NOREPLACE probe succeeds; a four-level host
instead must exhibit the kernel's exact native SIGSEGV outcome. Neither platform
branch skips the input or relaxes the admitted-address contract.

After the raw shadow fix, the focused command
`cargo --offline test -p reverie-elf-loader --test parity kernel_ -- --nocapture`
passed all four new test functions with 39 other parity functions filtered.
Evidence: `target/elf-loader/r5/kernel-controls-after-shadow.log`. This auditor
did not run the full suite; the parent owns the required final gates.

The additional mapping-size wrapper passed all three extent/reservation
subcontrols: one test passed, zero failed, 44 other parity tests filtered.
Evidence: `target/elf-loader/r5/kernel-mapping-size-with-reservation.log`.

## Initial argument and stack extent bound

For direct x86-64 ELF with ADDR_NO_RANDOMIZE, STACK_TOP is
`DEFAULT_MAP_WINDOW = 0x7ffffffff000`. `exec.c:410–438` budgets strings and
argv/env pointers within at most `3/4 * _STK_LIM = 6 MiB`; the copied execfn
also consumes this budget (`exec.c:1831–1840`). `setup_arg_pages` adds at most
128 KiB below copied string pages (`exec.c:698–711`). Tables need fewer than
1 KiB beyond the already-budgeted pointers (x86 platform, random bytes,
fixed auxv entries, two NULLs/argc and alignment). Therefore the top 8 MiB
`[0x7fffff7ff000, 0x7ffffffff000)` safely contains every possible initial stack,
including an unlimited or raised RLIMIT_STACK. Reserving this range is
conservative but has a source-derived bound; sampling one current stack VMA
is insufficient because preparation does not receive the eventual argv/envp.

## Argument stack, auxv tables and vDSO computations

| ID | Kernel computation and source lines | Matching C / Rust source and computation | Verdict and admitted/refused scope |
| --- | --- | --- | --- |
| S01 | `E:1422–1431`: filename is literal path for absolute/AT_FDCWD, otherwise `/dev/fd/N[/path]`. | `R:334–344`, Invocation::native_execfn duplicates the formula; `C:492–515,782` copies its exact bytes into AT_EXECFN. | Equivalent for supported execve/execveat invocations. Unsupported flags, NUL, empty path without AT_EMPTY_PATH are named InvalidInvocation refusals (`R:322–331`). |
| S02 | `E:543,553,1831–1834`: filename consumes strlen+1 stack bytes and defines bprm->exec. | `R:666–696` constructs a lookup-equivalent launcher path of the identical byte length, including NUL accounting; `C:580–582,782` verifies equal string extent before replacement. | Equivalent. PATH_MAX and launcher-too-long refusals preserve layout rather than relaxing length comparison. |
| S03 | `E:1425–1427,1232–1246`: AT_EMPTY_PATH uses the executable dentry for comm; other invocations use kbasename(filename). | `R:599–630` derives basename from pinned procfd dentry or native_execfn, truncates to 15 bytes; `C:783–784` PR_SET_NAME. | Equivalent for unambiguous pinned immutable dentry names. Ambiguous live-hardlink ` (deleted)` suffix gets AmbiguousDescriptorComm. |
| S04 | `mm/vma_exec.c:138–150`: temporary VMA ends at STACK_TOP_MAX, starts one page lower, p=end−sizeof(void *). | Launcher runs this kernel path; `entry.S:14–27` switches to private scratch, and `C:562–578,787` consumes and preserves the original stack. `R:445–504` generates the ordinary ELF64 launcher that delegates VMA construction to kernel exec. | Equivalent allocation; subsequent equality requires stack mappings not be replaced by loaded ELF. See S25. |
| S05 | `E:266–269`: snapshot RLIMIT_STACK for all exec calculations. | Same caller resource context reaches launcher exec; `R:358–382` snapshots AS/DATA only and `R:445–504` generates its image without changing stack limits. `C:553–789` consumes the resulting kernel context and has no setrlimit call. | Equivalent under the caller's identical resulting secureexec context; setid/filecap executable classes are refused as in S09. |
| S06 | `E:410–438`: max(ARG_MAX,min(3/4*_STK_LIM,stack_limit/4)) minus max(argc,1)+envc pointer bytes. | Delegated unchanged to kernel; argv/envp protocol `R:11–19`; C reads generated vectors (`C:562–569`) without changing count or strings. | Equivalent for the exact caller argv/envp. Both kernel execs enforce E2BIG at the same boundary because execfn lengths match. |
| S07 | `E:369–383`: argmin=p−limit; refuse p<limit; each string copy refuses p<argmin. | Kernel delegation uses the same caller argv/envp (`R:11–20`) and equal-length filename (`R:667–697`). `C:562–582` reads the resulting vectors and verifies execfn length without synthesizing a pointer budget. | Equivalent for admitted direct ELF invocations. |
| S08 | `E:448–526`: reverse string order, decrement p by NUL-inclusive strlen, split page copies at pos%PAGE_SIZE, page-start masks. | The generated launcher (`R:445–504`) delegates string copying with exactly the caller argv/envp (`R:11–20`). `C:562–569` follows original vectors; `C:773–782` changes only designated auxv values and execfn bytes. | Equivalent bytes, extents and ordering; C writes only designated auxv and execfn fields. |
| S09 | `E:1196–1208`: secureexec clamps stack rlimit to _STK_LIM before mmap layout. `E:1534–1583`, `security/commoncap.c:929–931,989–996` derive per_clear/secureexec from target inode. | `R:541,1223–1248` refuses any setid mode or present security.capability; `C:244–251` repeats it on fd 100. `R:503` creates the ordinary mode 0755 launcher. | Closed by `PrivilegedExecutable`. Own-ID setuid/setgid and empty-capability native controls execute, exact Rust refusal and preprepared runtime bypass diagnostic are mandatory; ordinary restored controls keep full parity. Inode-sensitive LSM context must yield identical resulting credentials/secureexec under the caller contract. |
| S10 | `E:1850–1854`: empty argv becomes one empty argv[0]. | Same kernel normalization reaches the generated launcher (`R:445–504`) under exact caller argv/envp contract (`R:11–20`); `C:562–566` accepts the resulting argc/argv unchanged. | Equivalent; libc-style nonempty-argv assumptions do not enter the loader. |
| S11 | `K:1001–1007`; `arch/x86/kernel/process_64.c:717–734`: enter ELF64 personality, clear TIF_ADDR32 and READ_IMPLIES_EXEC, rewrite orig_ax=__NR_execve and clear TS_COMPAT at 724–725; set PF_RANDOMIZE only without ADDR_NO_RANDOMIZE. | Launcher is ELF64 x86-64; `R:759–776,448–450` refuses non-ELF64/non-ET_EXEC template; `C:554–556` requires ADDR_NO_RANDOMIZE. Both kernel paths clear compat syscall state before ELF allocation. | Equivalent for admitted non-transitioning executable, including a compat caller becoming ELF64. Inherited READ_IMPLIES_EXEC is cleared on both starts. ADDR_LIMIT_32BIT does not re-enable TIF_ADDR32 or compat mmap for x64 ELF. |
| S12 | `arch/x86/include/asm/page_64_types.h:54,62–70`: STACK_TOP=DEFAULT_MAP_WINDOW for x86-64, not the larger five-level full-address-space limit. | Generated x64 launcher (`R:445–504,759–776`) delegates stack-top selection to kernel exec. `C:787` and `entry.S:146` preserve its original RSP; Rust does not invent another stack-top calculation. | Equivalent, including five-level hosts because both begin with x86-64 STACK_TOP. |
| S13 | `mm/util.c:341–354`: page-align STACK_TOP; PF_RANDOMIZE clear makes random subtraction zero. | The x64 launcher (`R:445–504`) delegates kernel stack randomization under caller ADDR_NO_RANDOMIZE precondition (`R:18–20`); `C:554–556` enforces that precondition at runtime. | Equivalent. ASLR-on is a named runtime refusal, not an admitted approximate layout. |
| S14 | `arch/x86/kernel/process.c:1021–1025`: with ADDR_NO_RANDOMIZE arch_align_stack(p)=p &~15, no random shuffling. | Equal filename extent (`R:667–697`) and caller vectors (`R:11–20`) delegate the same p to both kernel alignment calls. `C:554–556,787` enforces no randomization and preserves original RSP; `entry.S:14,146` saves/restores it. | Equivalent for all supported argv/envp lengths; no alignment tolerance. |
| S15 | `E:635–640`: arch_align_stack(stack_top), PAGE_ALIGN, reject range too large/too low for mmap_min_addr. | Launcher generation (`R:445–504`) delegates setup_arg_pages validation; equal filename (`R:667–697`) and caller vectors (`R:11–20`) preserve temporary argument extent. `C:562–582,787` consumes the resulting original pointers without another stack allocation. | Equivalent. Caller/kernel exec errors occur before loader main; preparation does not claim to replace native authorization/error ordering. |
| S16 | `E:642–648`: stack_shift=temporary_vm_end−stack_top; p,arg_start,exec subtract stack_shift. | Same kernel pointers are read/preserved at `C:562–582,787`; exact filename matching at `R:667–697` plus argv/envp contract `R:11–20` preserves p before the delegated shift. | Equivalent. |
| S17 | `E:653`: VM_STACK_FLAGS determines read/write, grow-down, accounting and allowed protections. | Template `loader.ld:20` PT_GNU_STACK FLAGS(6); target must have exactly one nonexecuting GNU_STACK (`R:1035–1042`; `C:595–601`). | Equivalent for every admitted non-X GNU_STACK flag combination; kernel does not use its PF_R/PF_W, memsz or p_align to size stack. |
| S18 | `E:660–663`: EXSTACK_DISABLE_X explicitly removes VM_EXEC. `K` main GNU_STACK scan chooses that value. | Template itself requests no X; preparation and C refuse main X/missing/duplicate headers (`R:1035–1042`, `C:595–601`). | Equivalent. Executable/missing/repeated stack is named ExecutableStack refusal, existing native/preparation controls cover it. |
| S19 | `E:664–665`: include mm->def_flags and temporary VM_STACK_INCOMPLETE_SETUP. | Launcher generation (`R:445–504`) delegates original kernel stack VMA flags with target non-X GNU_STACK policy (`R:1035–1042`). `C:562–578,787` and `entry.S:14–27,146` preserve that VMA while using independent scratch. | Equivalent for same caller context; no independent user mmap substitutes stack creation. |
| S20 | `E:669–696`: mprotect_fixup installs protections; relocate_vma_down shifts entire argument VMA; then clear incomplete flag. | Kernel exec of the generated launcher (`R:445–504`) performs the same stack protection/relocation, with target stack policy at `R:1035–1042`. `C:562–578,787` preserves its stack pointer and `entry.S:14–27,146` avoids writes through a private scratch stack. | Equivalent, subject to S25 mapping exclusion. |
| S21 | `E:698–711`: stack_expand=min(stack_limit&PAGE_MASK,copied_stack_size+131072); stack_base=vm_end−stack_expand. | Kernel delegation uses matching copied vectors (`R:11–20`) and filename extent (`R:667–697`). `C:73,562–578,787` uses independent 64 KiB scratch and retains kernel stack state; `entry.S:25` selects that scratch. | Equivalent, even tiny/unaligned/infinite stack limits; no additional initial-stack growth caused by loader instructions. |
| S22 | `E:713–714`: mm->start_stack initially p; expand_stack_locked(stack_base). | Generated launcher (`R:445–504`) delegates expansion with matching strings (`R:11–20,667–697`). `C:787` and `entry.S:14,146` retain original RSP; table counts remain identical as recorded in A31–A36. | Equivalent initial setup. |
| S23 | `arch/x86/mm/mmap.c:62–68,82–104,111–143`: ADDR_COMPAT_LAYOUT/sysctl selects legacy allocation; same stack rlimit and zero random offset determine mmap bases. `mm/mmap.c:941–950` defines default 1 MiB stack gap and boot override. | Launcher kernel computes mm layout; nonfixed C allocations use that allocator (`C:355,741–742,757`). `R:542,1251–1260` and `C:156–178,557` refuse every visible stack_guard_gap= override. `R:917,1063–1067,1316,1321–1330` / `C:201–218,344–365` enforce 16 GiB extents/reservations; `C:53–62` proves a free gap above the low band. | Equivalent for preserved ADDR_COMPAT_LAYOUT and supported default-gap host. Legacy searches begin above the low loader band; bounded top-down searches cannot reach low holes or fall back due to admitted geometry. See allocator proof below. Boot overrides and oversized mappings have mandatory exact refusal controls and full-parity boundary companions. |
| S24 | `K:1332–1343`: MMAP_PAGE_ZERO maps page zero R|X and attempts mseal after ELF tables. | Kernel applies same operation to launcher; C does not remove zero page and all ELF loads below 0x400000 are refused (`C:206–213`; `R:883–893`). | Equivalent for preserved MMAP_PAGE_ZERO, whether policy permits or rejects that page. ADDR_LIMIT_32BIT does not set TIF_ADDR32 in x64 SET_PERSONALITY, so does not move stack/mmap window. |
| S25 | `K:1013–1017` vs `K:1275–1282`: string stack pages exist during mapping but tables are generated afterward. | Existing complete tables are read at `C:562–578`; nonempty effective/file/BSS/whole-reservation guards are `C:206–232,344–365,368–405`. Rust matches at `R:909–957,1061–1073,1101,1293–1340` and repeated shadow validation `R:487`. | Closed by `InitialStackOverlap` for every nonempty range or first reservation intersecting source-derived top 8 MiB. Main/interpreter native overwrite controls and adjacent-lower-band full-parity companions execute; raw empty shadows are separately fixed and admitted. The actual default-gap host guard also prevents pre/post-table VMA growth from changing nonfixed allocation. |
| A01 | `K:154–166,177`: start from copied-string p and apply arch_align_stack. | Launcher generation (`R:445–504`) delegates the same kernel tables; exact path padding (`R:667–697`) and caller vectors (`R:11–20`) preserve string p. `C:562–578,787` reads and retains already-built tables. | Equivalent under identical argc/envc and admitted stack-preserving geometry. |
| A02 | `K:186–191`: platform allocation strlen(ELF_PLATFORM)+1 below p; copy exact string. `asm/elf.h:221`: x86_64. | Platform pointer/string stay unchanged in the default branch (`C:773–780`); Rust patch list (`R:631–655`) excludes type 15, and `R:667–697` preserves its allocation address through identical filename extent. | Equivalent, exact pointer and bytes. |
| A03 | `K:198–205`: optional base-platform allocation and copy; generic ELF_BASE_PLATFORM is NULL (`K:139–145`). | Launcher kernel exec (`R:445–504`) supplies the architecture allocation. `C:773–780` preserves type 24 if present and Rust patch list (`R:631–655`) excludes it; neither adds base-platform bytes or entries. | Equivalent: no x64 base-platform entry in Linux 6.17. |
| A04 | `K:210–214`: allocate 16 random bytes below p and copy kernel randomness. | Random pointer/bytes remain unchanged by `C:773–782`, whose only stack-string replacement is execfn. `R:631–655` excludes type 25 and `R:445–504` delegates its allocation to kernel exec. Test harness replaces the same 16-byte witness in both starts before first instruction. | Equivalent generation, length and address. Independent runs naturally have different random bytes; equality evidence explicitly normalizes only these bytes. |
| A05 | `K:225–233`; `asm/elf.h:331–337`: arch auxv comes first. | Initial kernel order is preserved by scanning/mutating only values (`C:573–578,769–782`); Rust patch list (`R:631–655`) contains scalar/string targets without inserting or reordering entries. | Equivalent number/order for same x64 host configuration. |
| A06 | `asm/elf.h:333–335`: AT_SYSINFO_EHDR is current mm->context.vdso when vdso64_enabled. | `C:769–771` writes relocated vDSO text address; `R:648–650` plan marks type 33 RelocatedVdso. | Equivalent after successful unsealed relocation; vDSO-disabled host has existing named runtime refusal (V01). |
| A07 | `asm/elf.h:336`; `arch/x86/kernel/signal.c:214–236`: AT_MINSIGSTKSZ=get_sigframe_size(), architecture global signal-frame capacity. | Type 51 is retained by `C:773–780` default branch and absent from Rust patch list (`R:631–655`); launcher kernel exec (`R:445–504`) supplies the same host value. | Equivalent for same kernel/CPU. Loading ordinary ELF does not recompute this global feature-size value. |
| A08 | `K:234`; `asm/elf.h:242`: AT_HWCAP boot CPU CPUID_1_EDX capability mask. | Type 16 stays in `C:773–780` default branch; `R:631–655` excludes it from the patch list and `R:445–504` delegates its host value to kernel exec. | Equivalent same host; no ELF-header-derived HWCAP branch. |
| A09 | `K:235`; `asm/elf.h:228`: AT_PAGESZ=4096. | Retain type 6; C/R PAGE constants are 4096 (`C:23`, `R:50`). | Equivalent admitted x86-64 Linux. |
| A10 | `K:236`: AT_CLKTCK=CLOCKS_PER_SEC. | Kernel type 17 remains unchanged in `C:773–780` default branch; Rust patch list (`R:631–655`) excludes it, with kernel-table creation delegated by `R:445–504`. | Equivalent same kernel. |
| A11 | `K:237`: AT_PHDR=main mapped-header address. | `C:712–716,775` chooses last file-containing load; `R:1102–1113,631–634` plans same address. | Main mapping audit covers exact formula; unmapped headers are named UnsupportedElf refusal. |
| A12 | `K:238`: AT_PHENT=sizeof(elf_phdr)=56. | Loader preserves type 4; target/template header gate enforces ELF64 size 56 (`C:260–262`; `R:770–776`). | Equivalent all admitted inputs, regardless unused ELF section-header fields. |
| A13 | `K:239`: AT_PHNUM=main e_phnum. | `C:776` replaces type 5; `R:636–638` uses layout.phnum. | Equivalent; zero/>128 counts get named header refusal. |
| A14 | `K:240`: AT_BASE=interpreter relocation adjustment, including zero or negative unsigned bias. | `C:718–748,778`; `R:640–642` InterpreterBase plan. | Main/interpreter audit covers full bias algorithm; stack does not reinterpret negative bias. |
| A15 | `K:166,241–243`: AT_FLAGS=0 except PRESERVE_ARGV0 on binfmt_misc recursive handoff. | Kernel initial type 8 retained; Invocation only describes direct execve/execveat ELF; no bprm interp flags synthesized (`R:309–332`; `C:773–780`). | Equivalent for direct binfmt_elf invocation. A binfmt_misc wrapper is a distinct caller path; caller must explicitly exclude interception/recursion or model it. No false claim that execveat AT flags become AT_FLAGS. |
| A16 | `K:244`: AT_ENTRY=biased main e_entry. | `C:777`; `R:645–646,1109` exact biased entry. | Equivalent within admitted mapped geometry; entry-range policy belongs main audit. |
| A17 | `K:245`: AT_UID=from_kuid_munged(current cred.user_ns,cred.uid). | Retain type 11 (`C:773–780` default); launcher kernel sees the same real UID/user namespace. `R:541,1223–1248` checks target credential-bearing metadata. | Equivalent under identical caller exec context; setid/filecap inputs refused with executed controls (S09). |
| A18 | `K:246`: AT_EUID=from_kuid_munged(...cred.euid). | Retain type 12 (`C:773–780` default), no credential syscall. `R:541,1223–1248` / `C:244–251` refuse credential-bearing target metadata. | Closed by `PrivilegedExecutable`; same-context ordinary inputs preserve exact EUID. Inode-sensitive LSM transition equality is a caller precondition. |
| A19 | `K:247`: AT_GID=from_kgid_munged(...cred.gid). | Type 13 remains unchanged in `C:773–780`; `R:631–655` excludes it. Target credential transitions are refused at `R:541,1223–1248` / `C:244–251` under the identical caller/user-namespace contract. | Equivalent same real GID/user namespace. |
| A20 | `K:248`: AT_EGID=from_kgid_munged(...cred.egid). | Retain type 14 (`C:773–780` default), no credential syscall. `R:541,1223–1248` / `C:244–251` refuse target metadata transitions. | Closed by `PrivilegedExecutable`; ordinary same-context inputs preserve exact EGID, including callers with unequal real/effective IDs. |
| A21 | `K:249`: AT_SECURE=bprm->secureexec. | Retain type 23 (`C:773–780` default); named target refusal at `R:541,1223–1248` / `C:244–251`. | Closed for setid/filecap inputs by `PrivilegedExecutable` with native/refusal controls. Every admitted start requires identical target/launcher resulting secureexec, including inode-sensitive LSM policy; unequal caller real/effective IDs alone are not refused because both ordinary execs evaluate the same context. |
| A22 | `K:250`: AT_RANDOM=u_rand_bytes pointer. | Original type 25 pointer and its 16-byte pointee are retained by `C:773–782`; Rust patch list (`R:631–655`) excludes 25, and exact filename matching (`R:667–697`) preserves stack layout. | Equivalent pointer with S01/S02 filename-length matching; only bytes are intrinsically nondeterministic. |
| A23 | `K:251–253`; `asm/elf.h:251`: AT_HWCAP2=elf_hwcap2 global kernel-enabled features. | Type 26 remains unchanged in `C:773–780` default branch and is absent from Rust patch list (`R:631–655`); launcher generation (`R:445–504`) delegates kernel value creation. | Equivalent same kernel. |
| A24 | `K:254–259`: optional AT_HWCAP3 and AT_HWCAP4. | Kernel architecture supplies no types 29/30. `C:573–578,773–780` scans and preserves existing entries without insertion; Rust patch list (`R:631–655`) adds neither type, with original kernel table creation delegated by `R:445–504`. | Equivalent absence for this architecture/source revision; future kernel additions are retained rather than misclassified. |
| A25 | `K:260`: AT_EXECFN=bprm->exec pointer. | Pointer is retained (`C:577`), same-length bytes replaced (`C:782`); R plans ExecFnString (`R:652–654`). | Equivalent pointer and target literal bytes under S01/S02; unlike scalar patched procfs auxv, saved execfn value still points to same stack address. |
| A26 | `K:261–264`: AT_PLATFORM=u_platform if platform exists. | Type 15 points to unchanged platform bytes through `C:773–780` default branch; Rust patch list (`R:631–655`) excludes it and exact filename matching (`R:667–697`) preserves its address. | Equivalent via A02. |
| A27 | `K:265–268`: optional AT_BASE_PLATFORM=u_base_platform. | Kernel supplies no type 24 here; `C:773–780` would preserve it unchanged and Rust patch list (`R:631–655`) excludes it. Launcher generation (`R:445–504`) delegates optional entry creation. | Equivalent absence via A03. |
| A28 | `K:269–271`: optional AT_EXECFD=bprm->execfd if bprm->have_execfd. `fs/binfmt_misc.c:223–231` sets preserve-argv0/have-execfd. | Direct-ELF protocol is explicit at `R:18–22`; Rust patch list (`R:631–655`) and C scan/default branch (`C:573–578,773–780`) synthesize no AT_EXECFD. fd 100 is private loader protocol, not kernel type 2. | Equivalent absence for direct ELF. binfmt_misc open-binary recursion must be outside caller's direct-ELF protocol, not silently treated as an execveat variant. |
| A29 | `K:272–274`: AT_RSEQ_FEATURE_SIZE=offsetof(struct rseq,end), AT_RSEQ_ALIGN=alignof(struct rseq). | Both entries remain unchanged in `C:773–780` default branch; Rust patch list (`R:631–655`) excludes types 27/28 and generated launcher (`R:445–504`) delegates kernel ABI values. | Equivalent same CONFIG_RSEQ and ABI; no loader runtime registration is performed. |
| A30 | `K:277–282`: zero remaining saved_auxv, then advance two words for AT_NULL. | C stops at AT_NULL without rewriting it (`C:573,769,773`); Rust patch list (`R:631–655`) excludes type 0. Kernel table creation is delegated through launcher generation (`R:445–504`), so original zero terminator/order survive. | Equivalent, including AT_NULL value 0; the unchanged comparator checks it exactly. |
| A31 | `K:284–285`: ei_index=written auxv words including AT_NULL; sp=p−ei_index*sizeof(elf_addr_t). | Generated launcher (`R:445–504`) delegates table allocation with equal filename extent (`R:667–697`). `C:573–578,769–782` and Rust patch list (`R:631–655`) mutate fixed-size values without changing entry count. | Equivalent when direct ELF has same optional auxv presence. |
| A32 | `K:287–288`; STACK_ROUND `K:133–136`: items=argc+envc+3; bprm->p=round_down(sp−items*8,16). | Original RSP is saved/restored (`entry.S:14,146`; `C:787`) with no pushes on it. Exact caller vectors (`R:11–20`) and filename matching (`R:667–697`) delegate identical kernel counts and rounding. | Equivalent integer counts and 16-byte rounding under all admitted argc/envc. Kernel pointer budget bounds counts below C argc capacity 0x100000 (`C:563`). |
| A33 | `K:303–308`: find_extend_vma_locked expands stack to final table p or fails; pointers can exceed the 128 KiB setup_arg_pages expansion. Native interpreter/vDSO allocation precedes this final growth (`K:1275–1282`). | Launcher kernel expands the same preserved stack before C uses scratch. `R:542,1251–1260` and `C:156–178,557` enforce default 1 MiB boot gap; `R:1331–1340` / `C:217–218` exclude ELF overwrites of top 8 MiB. | Equal final stack plus allocator equivalence for admitted default-gap host: max 8 MiB initial stack+1 MiB guard stays above the 128 MiB minimum mmap gap. Arbitrary boot gaps could change pre/post-table allocation ceilings; named `StackGuardGapOverride` closes that separate ordering defect with actual Rust/C guard controls. |
| A34 | `K:311`: store argc at final p. | Kernel stores argc during launcher exec (`R:445–504`) with caller argv contract `R:11–20`; `C:562` reads the stored value unchanged, and `C:787` / `entry.S:146` preserves original stack pointer. | Equivalent including normalized empty argv case. |
| A35 | `K:315–327`: argv pointers from arg_start; each increments by exact strnlen_user; trailing NULL; arg_end. | Caller argv contract is `R:11–20`; launcher generation (`R:445–504`) delegates kernel pointer/string construction. `C:564–566` reads argv/its terminator and `C:773–782` changes only designated auxv and execfn fields. | Equivalent pointer values, strings, terminator and mm arg fields under stack-preserving geometry. |
| A36 | `K:330–342`: env_start=arg_end; env pointers sequential string lengths; trailing NULL; env_end. | Caller envp contract is `R:11–20`; generated launcher (`R:445–504`) delegates string/vector construction with filename matching (`R:667–697`). `C:566–569` walks retained envp only to locate auxv. | Equivalent vector and mm env fields. |
| A37 | `K:345`: copy saved_auxv exact word extent onto stack. | C changes stack auxv values only (`C:769–782`), while kernel saved_auxv remains launcher origin. Rust patch list (`R:631–655`) names those mutations and launcher generation (`R:445–504`) delegates initial saved/stack auxiliary tables. | Stack equivalent; saved /proc/self/auxv retains launcher-template scalar values at mutation sites {3,5,7,9,33}. Ordinary qualifying fixtures require exactly that differing set; existing zero/negative-bias fixtures require exactly {3,5,9} or {3,5,7,9}, with independently checked equal values. Other admitted source values can coincide (for example e_phnum=3), so mutation sites are not a universal differing-value set. Every original comparator remains unchanged; procfs equality is not claimed. |
| V01 | `arch/x86/entry/vdso/vma.c:236–245`: select x64 vDSO image; vdso64_enabled=0 omits mappings entirely. | Rust gates x64 (`R:24–25`), generates its launcher (`R:445–504`) and names RelocatedVdso (`R:648–650`); host scope is explicitly delegated to README (`R:21–22`), without a Rust host probe. `C:428–467,653–661` requires native vvar+vdso and reads selected mapping geometry. | Equivalent selected image on supported vDSO-enabled hosts. vDSO-disabled host is existing named runtime refusal, not an executable-geometry mismatch disguised as parity. |
| V02 | `vma.c:147–154`: ask get_unmapped_area(NULL,0,image.size+__VDSO_PAGES*PAGE_SIZE,0,0); text_start=base+vvar span. | C derives actual whole span (`C:655–661`) and requests anonymous private PROT_NONE slot with the same size (`C:756–771`), moving components with preserved offsets. Rust launcher generation (`R:445–504`) delegates original special VMAs and `R:648–650` records only the resulting RelocatedVdso value. | Equivalent placement on supported x64 host. Flags add no file alignment/MAP_32BIT/shadow-stack guard; span<0x80000 prevents PMD-size anonymous alignment variation (`mm/mmap.c:844–852`). |
| V03 | `vma.c:159–165`: text special VMA image.size, RX, MAYREAD/MAYWRITE/MAYEXEC, optional sealed. | Full-VMA mremap at `C:469–489` preserves original kernel text object/protections. `R:445–504` delegates initial special mappings to kernel exec and `R:648–650` plans only text-address relocation, without substitute vDSO bytes. | Equivalent size/protection/object for unsealed host; CONFIG_MSEAL_SYSTEM_MAPPINGS refusal listed V09. |
| V04 | `vma.c:172`; `lib/vdso/datastore.c:99–104`: vvar at base, VDSO_NR_PAGES pages, R/MAYREAD/IO/DONTDUMP/PFNMAP and optional sealed. | Whole original vvar VMA is moved at `C:469–489`, preserving offset 0. Rust launcher generation (`R:445–504`) delegates kernel special-object creation; symbolic relocation plan (`R:648–650`) creates no anonymous vvar substitute. | Equivalent original special mapping and vm_pgoff; no anonymous emulation. |
| V05 | `vma.c:179–184`: vvar_vclock at VDSO_VCLOCK_PAGES_START(base), VDSO_NR_VCLOCK_PAGES size and R-only special protection flags. | Optional native component is recorded at `C:446–467` and moved with exact relative extent at `C:469–489,763–768`. `R:445–504` delegates original kernel layout and `R:648–650` records symbolic vDSO relocation without hard-coded vvar page counts. | Equivalent for configured native components; no hard-coded number of vvar pages. |
| V06 | `vma.c:193–194`: mm->context.vdso=text_start and vdso_image=selected image. `vma.c:80–88`: vdso_mremap updates context.vdso on move. | Actual mremap (`C:469–489`) invokes kernel context updater for temporary/final moves; `C:769–771` patches text address. Rust symbolic plan is `R:648–650`, with initial selected image/context delegated by launcher generation (`R:445–504`). | Equivalent mm context and image, with no stale cached base. |
| V07 | `lib/vdso/datastore.c:48–91`: faulted vvar pages are namespace/feature-dependent; relative timens page positions. | C moves original special VMA (`C:469–489`) and loader_main changes no namespace (`C:553–789`). Rust preparation (`R:528–664`) performs no namespace transition and delegates kernel-created special pages via image generation (`R:445–504`), preserving caller namespace and pgoff. | Equivalent page-selection semantics for same caller namespace. Continuous clock-data changes are live kernel state, not replaced payload. |
| V08 | `arch/x86/kernel/sys_x86_64.c:127–239`: allocator considers mappings, immediate-neighbor stack gaps and mmap base; native call follows main/interpreter mapping and precedes tables (`K:1275–1282`). | C moves old specials out of the search region, removes the shadow, then loads real main/interpreter (`C:667,682–748`) before the identical special-span request (`C:756–771`). Rust/C enforce default boot gap (`R:542,1251–1260`; `C:156–178,557`),16 GiB rounded extents/reservations (`R:1321–1330`; `C:201–218,344–365`) and initial stack exclusion (`R:1331–1340`; `C:217–218`). Hugetlb inode semantics are refused before header reads (`R:804–820`; `C:239–243`). | Equivalent allocator request/order for every admitted bounded geometry on supported default-gap host; the free-gap proof prevents low-hole/bottom-up divergence, and stack growth stays above the mmap search ceiling. Existing low/upper returned-address diagnostics still refuse unsupported actual destinations. Named oversized/boot/hugetlb controls execute without weakening any comparator. |
| V09 | `include/linux/mm.h:4141–4144`: CONFIG_MSEAL_SYSTEM_MAPPINGS turns VM_SEALED_SYSMAP into VM_SEALED. `mm/mremap.c:1665` refuses sealed VMA moves. | C diagnoses mremap failure as relocate vvar or vdso (`C:485–488`). Rust retains symbolic relocation (`R:648–650`) and explicitly refers to README host scope (`R:21–22`), with no invented preparation-time sealing probe. | Host-specific runtime refusal. Running host successful native/loader relocation controls demonstrate it is not this platform. Cannot label all Linux 6.17 configurations equivalent. |
| V10 | `vma.c:41`: vDSO image size page-aligned; summed vvar+vclock pages are contiguous per `vma.c:30,147–184`. | C validates page alignment (`C:451`), computes actual union/span and checks temporary band/maximum 0x80000 (`C:469–484,655–661`). Rust launcher generation (`R:445–504`) delegates original native geometry; plan `R:648–650` names its symbolic relocated text address instead of hard-coding special sizes. | Equivalent for accepted kernel special-map geometries; malformed/missing/duplicate/oversized special-map layout gets named runtime diagnostic. |
