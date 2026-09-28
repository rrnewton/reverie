# CPUID Tool dispatch candidate, V1

This is an authored, isolated source candidate based on the exact qualified timestamp V6 snapshot. It has not been compiled, tested, reviewed, applied to the live worktree, or qualified for parity. The separate completed hardware probe establishes the CPUID fault transport on this host; it does not qualify this implementation.

The candidate connects the existing CPL3 ELF Tool consumer to `Tool::handle_cpuid_event`. A subscribed consumer owns CPUID faulting on its actual vCPU, dispatches the original low 32-bit EAX/ECX inputs, and writes the returned four 32-bit results into zero-extended RAX/RBX/RCX/RDX. It uses the timestamp implementation's actual Guest/RPC/terminal driver and user-frame retirement. There is no new public API, dependency, scheduler change, fixed CPUID-table change, synthetic event, clock adjustment, or comparator change.

## Source and measured prerequisites

`BASE.json`, `SOURCE-CONTINUITY.json` and `SOURCE-MANIFEST.json` identify the complete V6 base and candidate. `SOURCE.patch` is the complete eight-path CPUID-only delta against V6, not a delta against current landed Reverie. Four paths are new modules. All existing test bodies are unchanged; the two existing test-containing files only gain module includes in their test sections. The actual ignored Cargo.lock is separately bound and identical to V6. The experimental transport-probe module is not in this candidate.

The V6 source originally rests on authored commit 79516661bf82d30ab2967c71834a6d47447b76ee, whose complete tree is identical to landed commit 44fcb1955f44547f50d724fe8f7d718215fed446 (https://github.com/rrnewton/reverie/pull/585). V6 timestamp changes are a separate, unlanded prerequisite. V6's component results do not transfer to the common instruction path changed here, and no previous review is this candidate's approval.

The completed probe TARGET is ec74be70657c064951f52b9d233b9b018444e41850bf41f1b28f0962082ebfcf. Its five observed phases passed; its one selected declaration entered five guest cases across three VMs. On the measured vendor kernel, system feature read returned count 1 and PLATFORM_INFO 0x80000000. Each of six vCPU state reads returned count 2 and six writes returned count 1. Armed CPL3 CPUID produced #GP(0) at unchanged RIP and inputs. An independent unarmed backend and the explicitly disarmed original returned the installed table. CLI retained its own #GP. Those exact observations and raw receipts remain in `cpuid-transport-probe-v1/final-v1`, linked by `PREREQUISITES.json`.

The upstream primary corpus is Linux 199c9959d3a9b53f346c221757fc7ac507fbac50, distinct from the measured vendor kernel. The corpus demonstrates VMX/SVM routing through `kvm_emulate_cpuid`, whose fault check precedes table lookup, register writes and RIP advance. It does not establish universal host availability. This candidate explicitly refuses subscribed startup when support/read/write/readback cannot be established.

## Ownership and failure behavior

`cpuid_instruction::Interception` is a private member of one KvmBackend. Before its first write it saves both full original MSR values. Feature and vCPU GET/SET operations require exact returned counts, not merely absence of errno. Admission writes support then enable and rereads both values. Only full equality marks the consumer enabled.

Any setup failure attempts restoration and returns the setup error together with restoration errors. Restoration attempts both original writes and a full readback, even after an earlier restoration failure. It clears the saved original only after all steps succeed. A failed restoration leaves an explicit unresolved owner and prevents a later subscribed consumer from assuming admission. No guest resumes after any of these refusals. Existing `Error::with_cleanup` and `Error::combine` retain the original/cleanup structure; no primary-only flattening is introduced.

Every actual ELF Tool-loop entry establishes its subscription before tracking/registration/start callbacks or guest instructions. New fork/thread vCPUs initialize unarmed; they independently establish their actual consumer. Same-vCPU exec keeps the admitted state: the current exec/bootstrap path does not rewrite these MSRs, and the real post-exec instruction returns through the same dispatcher. Host workers use the tool-less loop and disarm owned interception. Direct ELF, public direct vmcall and public non-ELF Tool loops likewise disarm an earlier owned state. A fresh unsubscribed backend makes no optional CPUID-fault capability query. The existing `Guest::has_cpuid_interception` reports the real executor's admitted state; it remains false for public non-ELF Guests.

## Fault selection and retirement

Only an enabled consumer, vector 13, zero exception error word, the actual CPL3 code/stack selectors, and the supported current long-mode page-table format can admit CPUID. The decoder reads the original instruction through the same authoritative user/executable page walker used by timestamp interception. It accepts ordinary legacy/REX prefixes and exactly opcode 0F A2 within 15 bytes, rejects LOCK and incomplete/overlength encodings, and does not fetch beyond a completed instruction. It never converts #UD or #PF into a CPUID callback. Unrelated faults retain the existing error path.

The Tool gets the saved user register file and low 32-bit input words. Its actual return controls all four output registers, with zero extension. RIP advances by the decoded length; other GPRs and RFLAGS are retained except architectural retirement of RF. The existing typed TF/#DB boundary reports the next RIP before another instruction executes. Segment restoration retains intentional injected FS/GS effects as in V6. No fixed instruction-time charge occurs in this backend.

The shared instruction callback keeps V6's real returning injections and ordinary/tail Exit or ExitGroup behavior. Explicit terminal success never resumes the instruction; group publication, consuming hooks and failure aggregation remain on the existing completion path. Fork/exec/other nonreturning process actions without a resumable syscall transport still refuse before side effects. This is the existing timestamp restriction applied to CPUID, not a new policy for ordinary syscalls. The new controls retain both genuine terminal success and unsupported-action refusal coverage.

## Accounting and scope

The retained H39ac callback context shows the actual `pre_handler_hook`, one `time.add_cpuid()` when configured, schedule-event RPC and `post_handler_hook`. The candidate invokes that existing callback once and adds no backend charge. The source-only controls observe actual guest branch clocks before/after returning host injections and RPCs, and compare successive instruction callbacks after real guest branches. A mixed CPUID/RDTSC/RDTSCP guest tests coexistence in the common dispatcher. These are not a proof of Detcore's cancellation reachability or whole-backend time parity.

The current installed CPUID table and indexed XSAVE policy are byte-identical to V6, including the unsubscribed RDTSCP capability decision. A subscribed Tool's explicit returned values are authoritative, as on ptrace. The root's separately retained CPUID+RDTSCP witness and full original same-run parity failures remain failures. Fresh same-run comparison is required after this candidate qualifies; a recovered callback count alone is not parity.

## Controls and limitations

`CONTROLS.json` lists 15 new declarations, the 37 unchanged V6 selectors, and three additional unchanged CPUID/XSAVE neighbors: 55 planned exact declarations. Their source bodies and limits are bound. This is a selection count, not an inventory or executed count. `PLAN.md` specifies the bounded future qualification and raw skip audit.

The real partial-transfer control asks the actual kernel to stop at an unsupported MSR, and checks the exact 1/2 read and 0/1 write refusals. The ownership control exercises actual admission, independent vCPU state, foreign-owner refusal, stale readback detection and restoration. These do not force failure of the second setup write or a rollback write. The all-error retention and retry ownership on those branches are source obligations; no fault-injected rollback runtime credit is claimed. A separately reviewed real syscall fault control may be needed if review requires that additional evidence. No simulator, kernel toggle, weakened count, or host-limit change is substituted.

The new guest controls use actual KVM and have no early KVM-availability success path. Existing neighbor skip behavior is unchanged; qualification must inspect raw stderr for every selector, including old timestamp-19. Native architecture reference for any disputed CPUID prefix/TF/RF behavior would be a separately bound experiment; neither decoder self-tests nor the old RDTSC native probe count as that measurement.

The current real Detcore cancellation witness timed out and demonstrated no timestamp cancellation. Root owns that investigation; this candidate neither repeats nor clears it. Public real-mode/CPL0 loops retain their current table execution and do not acquire a new CPUID Tool interception guarantee. No timer, inactive publisher, descriptor, or scheduler-design finding is discharged here.

## Preparation and goalpost check

Only author source formatting and static source/identity audits have run. The new code uses the existing `reverie::CpuIdResult` reexport and current SyscallInfo trait; there is no Cargo/lock change. Some preparation reads used nonexistent historical paths and were corrected using the actual frozen inventories; no build or evidence record was inferred from those reads. The first source-freeze checker also selected a cfg(test) import instead of the exact runtime test-module boundary and refused. Its script/result are retained under preparation; the corrected checker compares the complete actual test-module suffix without weakening the equality assertion.

All old assertion bodies, comparisons, limits, fixtures and selection requirements are preserved. No tolerance, exemption, skip, or expected-result relaxation was added. No old failure is renamed a pass. New terminal controls are additive copies adapted to CPUID and explicitly retain unsupported tails, status, cleanup, original-memory and no-continuation assertions. New MSR error tests require an observed error and its precise count; they do not label a crashed test as success. This is an author assessment, not independent source approval.
