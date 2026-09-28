"""Finish unlaunchable review drafts; no CLI reviewer or product command is run."""
from pathlib import Path
import hashlib
import json
import os
import stat
import sys

P = Path(__file__).resolve().parent
W = P.parent
L = W.parent.parent
H = Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-replay-prerequisites-20260918')
S = Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-parent-reader-support-20260916')
D = H / 'ignored/kvm-clock-design-review-preparation-v1'
Q = W / 'qualification-v2'
FIX = W / 'v2-fix'
SID = {'claude': '5d98403d-b8e9-42bf-8e2f-da7e7b70809c', 'native': '01a0b38e-9e3c-7883-9816-65199c048935'}
BASE = 'b5e2ab49cd99e5d456fa0238b8cebd75958c529f'

def require(ok, why):
    if not ok:
        raise RuntimeError(why)

def rec(path):
    p = Path(path)
    require(p.is_file() and not p.is_symlink(), str(p))
    h = hashlib.sha256()
    with p.open('rb') as f:
        for b in iter(lambda: f.read(1024*1024), b''):
            h.update(b)
    return dict(path=str(p), bytes=p.stat().st_size, sha256=h.hexdigest(), mode=stat.S_IMODE(p.stat().st_mode))

def put(path, value):
    with Path(path).open('x') as f:
        f.write(value if isinstance(value, str) else json.dumps(value, indent=2) + '\n')

require(sys.flags.optimize == 0, 'PYTHONOPTIMIZE=0 required')
binding = json.loads((P/'SOURCE-CONTEXT-BINDING.json').read_text())
extra_docs = []
for name in ['observer/observer.py', 'observer/before_exec.py', 'observer/unit_reference.py', 'observer/source-inputs.json', 'PHASES.json', 'HELPER-CONTINUITY.json']:
    path = Q/name
    lines = path.read_text().splitlines(True)
    chunks = []
    start = 0
    while start < len(lines):
        end = start
        size = 0
        while end < len(lines) and end-start < 160 and (size+len(lines[end].encode()) <= 11000 or end==start):
            size += len(lines[end].encode())
            end += 1
        out = P/'common/chunks'/('caller-complete-'+name.replace('/','-').replace('.','-')+f'-{start+1:05d}-{end:05d}.txt')
        put(out, ''.join(lines[start:end]))
        chunks.append(dict(**rec(out), first_line=start+1, last_line=end))
        start = end
    extra_docs.append(dict(name='caller-complete-'+name, document=rec(path), lines=len(lines), chunks=chunks, stage_order=2))

goalpost = '''GOALPOST-MOVING REVIEW RULE

Adversarial reviewers must look explicitly for goalpost moving. We are extremely skeptical of any goalpost moving. YOU DO NOT CLEAR THE BAR BY SIMPLY LOWERING THE BAR.

Treat each of these as an explicit review target:
- weakening an assertion so a test passes
- widening a tolerance · adding an exemption · skipping a case · relaxing a comparator
- renaming or relabelling so a failure reads as a pass
- deleting a check rather than satisfying it'''

own_text = {
 'claude': '''Your own earlier clock design report and its distinct coverage repair are included. Preserve your original false complete-read claim, the failed 155/163 audit and later eight-chunk repair as separate facts. The combined coverage is 155 + 8 distinct chunks; it does not make the first audit pass or prove that later source reads followed the listed order. Reassess your own F1–F7 against actual implementation, especially deferred request retirement/arming, pinned-event activation failure and liveness, meaningful post-stop launcher branches, the missing-PT_INTERP failure boundary, and both real guest branches. Your prior verdict was about an earlier design, not this source. No other reviewer's report or verdict is supplied.''',
 'native': '''Your own earlier clock design report and its custody are included. Reassess your own High on the injected initial-exec route bypassing ordinary request observation, and the requirement that all pre-post-exec requests be retired exactly once with no later timer callback or physical arming failure. Your prior test-plan objection required hard zero-callback assertions, including a timer requested inside the intercepted exec. Assess the actual implementation and controls rather than accepting author claims. Your prior verdict was about an earlier design, not this source. No other reviewer's report or verdict is supplied.'''
}

coverage = {}
for lane in ('claude','native'):
    old = json.loads((P/lane/'COMPLETE_READS.DRAFT.json').read_text())
    if lane == 'native':
        docs = old['documents']+extra_docs
        stats = dict(mandatory_documents=len(docs), mandatory_chunks=sum(len(d['chunks']) for d in docs), mandatory_lines=sum(d['lines'] for d in docs))
        index = dict(documents=docs, **stats)
    else:
        docs = [dict(name=d['name'], document=d['source'], chunks=d['chunks'], lines=sum(c['last_line']-c['first_line']+1 for c in d['chunks'])) for d in old]+extra_docs
        stats = dict(mandatory_documents=len(docs), mandatory_chunks=sum(len(d['chunks']) for d in docs), mandatory_lines=sum(d['lines'] for d in docs))
        index = [dict(name=d['name'], source=d['document'], chunks=d['chunks']) for d in docs]
    coverage[lane] = stats
    put(P/lane/'COMPLETE_READS.V2.DRAFT.json', index)
    put(P/lane/'READ-INDEX.V2.DRAFT.md', '\n'.join(f'{i}. {d["name"]}: {d["document"]["path"]} ({d["lines"]} lines)\n'+''.join('   '+c['path']+'\n' for c in d['chunks']) for i,d in enumerate(docs,1)))
    prompt = f'''PREPARATION ONLY — DO NOT DISPATCH THIS DRAFT.
Final committed head/tree and completed qualification must replace the explicit pending fields before root can authorize a launch. No reviewer has been launched for this packet.

Continue only your own existing session {SID[lane]}. Independently review the WHOLE initial-command clock and syscall-injection correction in rrnewton/reverie, base {BASE} .. [PENDING COMMITTED HEAD], tree [PENDING TREE]. The exact corrected source target is {FIX/'TARGET.json'} (SHA256 c01fe71e9f784013be5c657d20e9578467f9b463efe0fb70365e9eefceda34eb); full patch SHA256 50580ff2751dad2e24a2fc86aa5ca7274b13ef0ee6761514ea383a8e360e410e. Authored base 3d4a401e and landed b5 have identical tree 73024912837cbbeaa70b6d2173f5b596eebedcb3. Finalization must bind actual committed-source equality; a source-manifest identity alone is not an exact-head verdict.

Grounding comes first. Read your own GROUNDING_AUTHENTICATION and its source bindings before the new proposal. Your own model-visible reads have been reauthenticated in the required order: complete Reproducible Containers paper including appendix, complete scheduler including tests, PROJECT_VISION, roadmap, then proposal. Hermit context is 97c39b308efd854693038f24962484182d932f27; its 8,174-line scheduler is the exact blob 20c3c4296223d1aa03ca28e2f45514ad0a72bff6 / SHA256 a5da44ec706ad656230cf78aa5742fc40c5339a14aad30410b880dc18b576fa3 that YOU read in full. Current vision and roadmap bytes also match your own primary readings. No author's or peer's grounding is borrowed. If those source identities fail, stop before reading the proposal; do not replace primary material with a summary. Current Hermit repository instructions and both review skills are in the mandatory index.

{own_text[lane]}

Read every mandatory chunk in READ-INDEX.V2.DRAFT.md / COMPLETE_READS.V2.DRAFT.json. This includes the complete six changed files, full diff, injection delta, explicit oracle delta, two-import correction, relevant complete Guest/Tool/launch/Hermit time paths, all new test bodies, old clock tests, retained raw observations, full finite caller/observer and preserved failed results. Before files and the entire frozen source tree remain available for adaptive read-only follow-up. Kernel input identities are bound; complete relevant functions and call-site context are mandatory, while entire kernel files are available for follow-up. Upstream Linux 199c9959 source is not a claim about running vendor-kernel identity.

Try to refute these concrete contracts:
1. Only the initial command launcher interval is excluded. Identify the exact stopped-child creation, command provenance, perf enable-on-exec, first committed exec and activation-before-Tool-post-exec transitions. Function launch, attach, clone, failed/later exec and loader entry must retain faithful continuous accounting. There must be no constant subtraction, per-exec reset, freeze of true guest work, rounded time or comparator relaxation.
2. Short/artificial timer requests from handle_thread_start and an intercepted initial exec cannot send a physical notification or arm during the excluded interval. Every pre-post-exec request is retired exactly once on normal AND injected committed exec, with zero later callback and no stale-request arming failure. A new Tool post-exec request must remain live. Trace activation errors and pinned PMU health from implementation to actual witness; an exit code alone is insufficient.
3. Distinguish a real pending original syscall, the same original, a different original and no pending original through ordinary injection, reinjection, same/different fast tail, post-exec nonreturning tail, emulated success/error, later instruction callback, converted stops, injected frames, exec replacement and private-syscall interruption. Do not step or replay guest instructions merely to skip an original syscall that does not exist. Preserve actual kernel effects, errno/partial results, state consumption, and existing special legacy-vsyscall handling. Trace mutation ownership and cancellation, not just function names.
4. Inspect each test oracle. Actual guest branch counts 1/65 and 65/129 stay exact. The author changed post_exec 1 to 0 and the first post-exec Rcbs(1) timer observation 2 to 1, explicitly arguing that prior extra branches were backend entry replay. Review the real RIP/code/step/restore/counter evidence independently: the old actual [3,67] versus required [1,65] test remains a failure. Determine whether the new temporal assertions correct a causal error or lower the bar. Do not assume the author's explanation settles this.
5. Lower-layer tests use real freestanding guests with five-second inner waits. Distinguish declaration counts, physical modes and full Hermit end-to-end/parity evidence. Write controls check three meaningful arguments, not all six raw registers. Mapping/frame rejection neighbors do not establish every positive activated LiteInst path, converted mapping tail, interrupted private syscall/restart or another architecture. Inspect early-return/skip behavior, including existing precise-timer and legacy tests; count only what actually ran. Do not invent coverage from a passing libtest record.
6. Against the grounded scheduler, state allowed inputs, the point at which time/events become eligible, affected task states, resource/run-queue memberships, turn/clock behavior and backend scope. Determine whether host arrival or callback order can leak into time or delivery. State separate conclusions on determinism and Linux/POSIX semantics. No new scheduler or signal activation is present or approved by this clock review.

Evidence status at preparation: the original v2 run attempted two phases, accepted metadata then compile raw101; all 38 tests and all later phases were unrun. Exact errors were private args::PathPtr and redundant std::io::Write imports. The successor changes those imports only. Its 45-phase / 38-declaration plan is not a result. [PENDING FINAL QUALIFICATION RECORDS: exact source/ELF/command/plan/result bindings, actual inventory, all raw outcomes, elapsed/accounting/skip audit and lease completion.] Preserve every earlier diagnostic/caller/compiler refusal. The old debugger wrapper raw125 and failed mapped-page reads remain failed; task-bound read(fd) counts and successful step/restore evidence are narrower observations. The measured original launcher interval was 223 branches / 2,230 ns; older 2,270 ns is a distinct historical run, not a universal constant. Original full log comparison remains strict and retained.

{goalpost}

Read only. Do not edit source, launch tests/builds/guests, acquire a cache lease, fetch network content, write GitHub or dispatch another reviewer. Use only the actual permitted read tools. Follow up source questions within the bound local files. Return findings first with severity, precise file:line, reachable cause, impact and necessary correction. Explicitly assess all four goalpost checks, then separate determinism, Linux/POSIX, evidence, residual risks and unmeasured states. Require an unambiguous WHOLE-CHANGE verdict approve or changes requested at the final exact base/head, not merely approval of the two imports, source delta or test plan. A raw-zero reviewer exit and complete reading do not constitute approval. Report missing or truncated reads honestly and list any additional evidence needed; hashes, filenames and summaries are not reading credit.
'''
    prompt_name = 'prompt.DRAFT.txt' if lane == 'claude' else '01-source.prompt.DRAFT.txt'
    put(P/lane/prompt_name, prompt)
    target = dict(scope='Initial-command clock and syscall-injection whole-source review; draft, no launch authority', stage='01-source', session_id=SID[lane], base=BASE, head=None, tree=None, frozen_source=str(FIX/'source'), expected_actual_model='claude-opus-5[1m]' if lane=='claude' else 'gpt-5.6-sol', source_target=rec(FIX/'TARGET.json'), patch=rec(FIX/'SOURCE.patch'), grounding=str(S/'ignored/kvm-timer-review-grounding-v2-20260918') if lane=='claude' else str(S/'ignored/kvm-timer-native-cli-review-v1-20260918'), **stats, final_qualification_pending=True, executable_target_deliberately_absent=True)
    put(P/lane/'TARGET.DRAFT.json', target)

    # Each lane receives its own records only. Common source trees contain no
    # concurrent reviewer outputs. Do not sweep the sibling review directory.
    records = {r['path']:r for r in binding['files']}
    def bind(path):
        r=rec(path);records[r['path']]=r
    for doc in docs:
        bind(doc['document']['path'])
        for chunk in doc['chunks']: bind(chunk['path'])
    for r in json.loads((P/lane/'GROUNDING_INPUTS.json').read_text()): bind(r['path'])
    for row in json.loads((P/'common/KERNEL-INPUTS.json').read_text()): bind(row['source']['path'])
    for row in binding['before']:
        if row['present']: bind(row['file']['path'])
    for path in [P/'SOURCE-CONTEXT-BINDING.json', P/'HELPER-CONTINUITY.json', P/'common/KERNEL-INPUTS.json', FIX/'TARGET.json', FIX/'SOURCE-MANIFEST.json', FIX/'CONTINUITY.json', W/'qualification-v1/HISTORY.json', Q/'CALLER-TARGET.json', Q/'SETUP.json', Q/'CONTEXT.json', Q/'HISTORY.json', Q/'CALLER-DELTA.patch', W/'qualification-v1/final-v1/TARGET.json', W/'qualification-v1/final-v1/INPUTS.json']:
        bind(path)
    for path in sorted((P/lane).iterdir()):
        if path.is_file(): bind(path)
    put(P/lane/'inputs.DRAFT.json', dict(files=sorted(records.values(),key=lambda r:r['path']), symlinks=binding['symlinks'], immutable_preparation_only=True, result_inputs_pending=True))

put(P/'DISPATCH-PLAN.md', f'''# Prepared independent clock source reviews — not authorized to launch

Both lanes resume their own actual grounded session. No model override is supplied. Current exact corrected source is c01fe71e / full patch 50580ff2, against landed Reverie {BASE}. Final head, tree and qualification outcomes are absent intentionally; neither lane has TARGET.json, inputs.json, executable prompt or COMPLETE_READS.json. The prepared scripts therefore cannot dispatch this draft. Finalize in a distinct final-v1 subdirectory after root provides those bindings; preserve these draft files.

Grounding has been independently reconstructed from each session's own 39 complete model-visible primary chunks in canonical order. Scheduler source at H 97c39 is byte-identical to those readings, as are current vision/roadmap sources. Historical native on-request/read-only policy and Claude's later 155/163 source-read failure plus separate eight-chunk repair remain disclosed. Later source-index ordering is not the canonical grounding order.

After finalization and explicit root inspection, the concrete prospective invocations are:

    with-proxy env PYTHONOPTIMIZE=0 /usr/bin/python3 -B <final-v1>/claude/launch-review.py
    with-proxy env PYTHONOPTIMIZE=0 /usr/bin/python3 -B <final-v1>/native/launch-guard.py

The entire command is under with-proxy; native's child inherits that route. Admit sequentially: fresh all-UID census before the first launch, inspect its actual effective session/model/policy initialization, then a fresh census before the second launch. Each census refuses at 15 top-level CLI agents and rejects a live resume of that exact session. Do not infer capacity from this preparation or from a stale count. Root's approval of preparation is not execution authority.

Claude remains plan mode, with requested Read/Grep/Glob tools, same session 5d98403d-b8e9-42bf-8e2f-da7e7b70809c and expected actual claude-opus-5[1m]. The unchanged launcher has 1,800 seconds wall, TERM then 10-second KILL, and 64 MiB per raw output file. Its actual init may advertise extra MCP availability; distinguish that from actual use, which must remain within the three allowed read tools. The actual init/session/model/permission evidence must be inspected promptly.

Native remains approval never / sandbox read-only, same session 01a0b38e-9e3c-7883-9816-65199c048935, expected actual gpt-5.6-sol. Unchanged run_stage gives 1,800 seconds wall and 64 MiB combined raw stdout/stderr, TERM then three-second KILL. The actual persisted turn_context must prove model, session, cwd and never/read-only; command flags alone are not effective-policy evidence. Outer PYTHONOPTIMIZE=0 protects assert-using retained code. No model override, writes, extra reviewer, build/test/guest or lease is permitted.

Before/after input bytes, modes and symlink identities remain checked. Collect both owned exec handles through terminal status; retain all raw streams, failures, capacity records and actual process identity. Run the existing terminal audits under optimization zero. Claude audit changes only its descriptive scope string; native helpers are byte-identical. Hashes/delta are in HELPER-CONTINUITY.json. No acceptance predicate is relaxed.

Literal complete chunk coverage is required, with failed/truncated reads preserved. Native coarse stage-order output does not establish full listed-source order; report actual first-read order separately. Claude assistant prose and terminal result are retained separately, with explicit event-based report extraction. A missing read may require a separately authorized focused continuation, not a claimed complete audit. No peer findings/verdicts cross lanes. Raw zero, full coverage and unchanged inputs are custody facts, not approval.

Final binding checklist: actual Git base/head/tree and full six-path equality; corrected source manifest including ignored lock distinction; exact completed 45-phase evidence and attempted/passed/ignored populations; retained fresh lib/integration ELF and local library records; all raw selected outputs; unchanged caller/limits; preserved original compile101/no-tests packet and earlier baseline/debugger failures; current helper/prompt hashes; fresh prelaunch all-input authentication. If source changes, replace the target with a separately frozen successor rather than editing a running review packet. No broad additional qualification is requested by this preparation.
''')
put(P/'REPORT.md', f'''# Clock source review preparation

The two independent continuations are prepared, not launched. Corrected source target c01fe71e9f784013be5c657d20e9578467f9b463efe0fb70365e9eefceda34eb and complete patch 50580ff2751dad2e24a2fc86aa5ca7274b13ef0ee6761514ea383a8e360e410e are bound against landed b5's exact base tree. All actual source files and aliases were authenticated. Complete source/head equality and current qualification outcomes remain pending; no approval is inferred.

Both own-session primary readings are independently authenticated: 39 complete chunks each, ordered paper → entire 8,174-line scheduler → vision → roadmap, before proposal. H 97c39 scheduler blob and current vision bytes match those readings. Each lane includes only its own earlier clock findings and custody. The Claude original failed coverage and separate repair remain visible.

The drafts cover all six changed files and full test bodies, base copies, full patch and explicit oracle/two-import deltas, relevant complete launch/Guest/Tool/Hermit time paths, bounded complete kernel functions with source identities, actual retained observation streams, and the entire finite caller/observer. The source index has {coverage['claude']['mandatory_documents']} documents / {coverage['claude']['mandatory_chunks']} chunks / {coverage['claude']['mandatory_lines']} lines for Claude and {coverage['native']['mandatory_documents']} / {coverage['native']['mandatory_chunks']} / {coverage['native']['mandatory_lines']} for native. This is a reading population, not a test population or claim of new reading by either reviewer.

The original v2 metadata passed and compile failed raw101; all 38 test declarations were unrun. The two-import successor preserves every production/test assertion and guest byte. Its planned 45 phases and 38 declarations are not credited as execution in this packet. Old [3,67] versus [1,65] failure and the debugger raw125 stay failures. Post-exec 0 and timer 1 are explicitly changed causal oracles for independent challenge; real guest 1/65 and 65/129 stay exact. This review preparation does not establish whole Hermit parity or signal/timer integration.

All executable launch helpers retain the inspected cap-15 admission, actual-policy checks, 1,800-second / 64-MiB controls and no model override. Only the Claude audit's scope description changes. TARGET.json and runnable prompts are intentionally absent. DISPATCH-PLAN records prospective commands, result-custody checks and remaining final bindings. No source/index/HEAD mutation, product operation, build, test, lease, network access or external CLI launch was performed.
''')
records=[]
for p in sorted(P.rglob('*')):
    if p.is_file() and not p.is_symlink(): records.append(rec(p))
put(P/'PREPARATION-INPUTS.json', dict(files=records, symlinks=[], limitation='Preparation custody only; source target and each lane draft indexes bind external inputs separately. No final qualification or committed-head claim.'))
put(P/'READBACK.json', dict(report=rec(P/'REPORT.md'), plan=rec(P/'DISPATCH-PLAN.md'), source_binding=rec(P/'SOURCE-CONTEXT-BINDING.json'), helpers=rec(P/'HELPER-CONTINUITY.json'), inputs=rec(P/'PREPARATION-INPUTS.json'), source_target=rec(FIX/'TARGET.json'), qualification_pending=True, committed_head_pending=True, lanes={lane:dict(prompt=rec(P/lane/('prompt.DRAFT.txt' if lane=='claude' else '01-source.prompt.DRAFT.txt')), target=rec(P/lane/'TARGET.DRAFT.json'), inputs=rec(P/lane/'inputs.DRAFT.json'), coverage=coverage[lane]) for lane in ('claude','native')}, execution_count=0, product_mutations=0))
print(json.dumps(rec(P/'READBACK.json'),indent=2))
