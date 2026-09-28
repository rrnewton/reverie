"""Freeze the concrete review caller; this never launches an external session."""
from pathlib import Path
import difflib
import hashlib
import json
import os
import stat
import sys

F=Path(__file__).resolve().parent;P=F.parent;W=P.parent;Q=W/'qualification-v2';RESULT=Q/'final-v1'
S=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-parent-reader-support-20260916')
LEG=Q/'legacy-interpretation-v1';STRICT=LEG/'strict-count-addendum-v1'
HEAD='a9449f8ee22a17ba87a106c2af957a938fa57c45';TREE='da3e0a5c13bbd1b95adca1bc3ac0037b89494544'
BASE='b5e2ab49cd99e5d456fa0238b8cebd75958c529f';URL='https://github.com/rrnewton/reverie/pull/588'
SID={'claude':'5d98403d-b8e9-42bf-8e2f-da7e7b70809c','native':'01a0b38e-9e3c-7883-9816-65199c048935'}

def req(ok,why):
    if not ok:raise RuntimeError(why)
def rec(path):
    p=Path(path);req(p.is_file() and not p.is_symlink(),str(p));h=hashlib.sha256()
    with p.open('rb') as f:
        for b in iter(lambda:f.read(1024*1024),b''):h.update(b)
    return dict(path=str(p),bytes=p.stat().st_size,sha256=h.hexdigest(),mode=stat.S_IMODE(p.stat().st_mode))
def put(path,value):
    path=Path(path);path.parent.mkdir(parents=True,exist_ok=True)
    with path.open('x') as f:f.write(value if isinstance(value,str) else json.dumps(value,indent=2)+'\n')
def check(row):
    actual=rec(row['path']);req(all(actual[k]==row[k] for k in actual),str(row['path']));return actual

req(sys.flags.optimize==0,'optimization must be disabled')
proof=json.loads((F/'COMMITTED-SOURCE-PROOF.json').read_text());req(proof['head']==HEAD and proof['tree']==TREE and proof['all_tracked_bytes_types_modes_equal'],'commit proof not ready')
qproof=json.loads((F/'Q2-EVIDENCE-READBACK.json').read_text());req(qproof['all_phases_terminal'] and not qproof['all_phases_accepted'],'wrong Q2 state')
req(rec(LEG/'TARGET.json')['sha256']=='56ac45bc8eed84c6fb2bfcab134c459f8f5ee1220f131f421b01ce50ae466936','legacy drift')
req(rec(STRICT/'TARGET.json')['sha256']=='7ce907a4d54e5a9bcce5d18c4c0e4dc39ad8f2e3358fdd53612ede5db07fa770','strict legacy drift')
put(F/'SOURCE-AND-EVIDENCE.md',f'''Review target: {URL}, Reverie {BASE}..{HEAD}, tree {TREE}.

COMMITTED-SOURCE-PROOF verifies all 2,632 tracked entries against actual qualified source: 2,622 regular files, seven literal aliases and three unchanged gitlinks represented by empty directories. The sole extra qualification input is ignored Cargo.lock, 67,056 bytes, SHA256 1c09663e46bf21ad7c07eedd7821cccb72ae21f42485192649ff5473962bc856. It is not a committed source file. Four production files change; two test files are added. The actual Git patch is mandatory; the original prepared patch and equality proof remain bound.

Q2 attempted all 45 phases, accepted 44 and preserved one reader refusal. It qualified 37 of 38 declarations; the legacy payload returned zero and printed one exact test/suite success, but remains accepted=false because 26 ANSI tracing lines violated the original JSON-lines reader. No product test was retried. Original compile101 with all tests unrun remains preserved. Both selected fresh=false harnesses and emitted local libraries were retained before cache reuse.

The separate offline legacy interpretation checks four exact lifecycle records plus all 26 diagnostic lines and real time interception. The first proposed reader equated boolean/float counts with integer1; its preserved strict successor adds an explicit integer check and refuses all 16 corrupted transcripts. Those are offline evidence probes, not product tests or replacement qualification. Review their goalpost implications explicitly.

All 24,396 immutable Q2 input files and 37 aliases were reauthenticated. Each of the 45 actual plans, receipts, raw outputs, limits and terminal/accounting records was bound. The review index contains all 38 raw test stdout/stderr streams, the complete 45-outcome projection and current clock/mode records; complete original RESULTS and observer records remain available. Service CPU totaled 344.439192 seconds, observer wall 289.910501011 seconds, test payload wall 0.286275571 seconds.

This is a finite component qualification, not a Hermit run or full same-run backend parity. Existing precise-timer parent success does not independently authenticate both child modes. Positive activated-frame, converted-mapping-tail and interrupted private-syscall paths remain unclaimed. The historical 223-branch launcher measurement and entry-replay observations are distinct from current qualification. No source approval follows from this record.
''')

new_docs=[]
def doc(name,path,stage=3):
    path=Path(path);lines=path.read_text().splitlines(True);parts=[];start=0
    while start<len(lines):
        end=start;size=0
        while end<len(lines) and end-start<160 and (size+len(lines[end].encode())<=11000 or end==start):
            size+=len(lines[end].encode());end+=1
        p=F/'common/chunks'/f'{name}-{start+1:05d}-{end:05d}.txt';put(p,''.join(lines[start:end]));parts.append(dict(**rec(p),first_line=start+1,last_line=end));start=end
    result=dict(name=name,document=rec(path),lines=len(lines),chunks=parts,stage_order=stage);new_docs.append(result);return result

committed=doc('actual-committed-source-diff',F/'COMMITTED-SOURCE.patch',2)
for name,path in [
 ('source-evidence',F/'SOURCE-AND-EVIDENCE.md'),('actual-pr-body',S/'ignored/kvm-clock-pr-body-20260918.md'),
 ('q2-report',RESULT/'REPORT.md'),('q2-all-outcomes',F/'Q2-OUTCOME-SUMMARY.json'),('q2-all-test-raw',F/'Q2-RAW-SELECTED-OUTCOMES.txt'),
 ('q2-clock-observations',RESULT/'CLOCK-OBSERVATIONS.json'),('q2-mode-observations',RESULT/'MODE-OBSERVATIONS.json'),
 ('q2-artifacts',Q/'retained-binaries/ARTIFACTS.json'),('q2-artifact-binding',Q/'retained-binaries/BINDING.json'),
 ('legacy-original-refusal',Q/'controls/test-38/result.json'),('legacy-interpretation-report',LEG/'REPORT.md'),
 ('legacy-count-correction',STRICT/'REPORT.md'),
 ('legacy-test-helper',W/'v2-fix/source/reverie-ptrace/src/testing.rs')]:doc(name,path)

legacy_inputs=json.loads((STRICT/'INPUTS.json').read_text())
legacy_records=legacy_inputs if isinstance(legacy_inputs,list) else legacy_inputs['files']
for row in legacy_records:check(row)
stats={};targets={}
for lane in ['claude','native']:
    out=F/lane
    helpers=['launch-review.py','audit-final.py'] if lane=='claude' else ['launch-guard.py','run_stage.py','audit_source.py','audit_split_reads.py']
    for name in helpers:put(out/name,(P/lane/name).read_text())
    current=json.loads((out/'UNREAD-SOURCE-DOCUMENTS.json').read_text())['documents']
    normalized=[]
    for row in current:
        if row['name']=='full-change':normalized.append(committed);continue
        source=row.get('document',row.get('source'));normalized.append(dict(name=row['name'],document=source,lines=len(Path(source['path']).read_text().splitlines()),chunks=row['chunks'],stage_order=row.get('stage_order',2)))
    reuse=json.loads((out/'REUSED-READS.json').read_text())
    summary='Complete unchanged documents already read by THIS session; no new reading or approval implied.\n\n'+''.join(f'- {r["name"]}: {r["lines"]} lines; SHA256 {r["current_source"]["sha256"]}; current {r["current_source"]["path"]}; own prior {r["prior_source"]["path"]}\n' for r in reuse['documents'])
    put(out/'READ-REUSE-SUMMARY.md',summary)
    reuse_doc=doc(lane+'-reuse-summary',out/'READ-REUSE-SUMMARY.md',1)
    documents=[reuse_doc]+normalized+[d for d in new_docs if d is not committed and not d['name'].endswith('-reuse-summary')]
    count=dict(mandatory_documents=len(documents),mandatory_chunks=sum(len(d['chunks']) for d in documents),mandatory_lines=sum(d['lines'] for d in documents));stats[lane]=count
    index=dict(documents=documents,**count) if lane=='native' else [dict(name=d['name'],source=d['document'],chunks=d['chunks']) for d in documents]
    put(out/'COMPLETE_READS.json',index)
    put(out/'READ-INDEX.md','\n'.join(f'{i}. {d["name"]}: {d["document"]["path"]} ({d["lines"]} lines)\n'+''.join('   '+c['path']+'\n' for c in d['chunks']) for i,d in enumerate(documents,1)))
    draft=(P/lane/('prompt.DRAFT.txt' if lane=='claude' else '01-source.prompt.DRAFT.txt')).read_text()
    prompt=draft[draft.index('Continue only your own existing session'):]
    prompt=prompt.replace('[PENDING COMMITTED HEAD]',HEAD).replace('[PENDING TREE]',TREE)
    prompt=prompt.replace('Finalization must bind actual committed-source equality; a source-manifest identity alone is not an exact-head verdict.',f'The actual PR is {URL}. COMMITTED-SOURCE-PROOF.json binds all qualified tracked bytes/types/modes to this exact head; ignored Cargo.lock and three unchanged gitlinks are explicitly separate.')
    prompt=prompt.replace('READ-INDEX.V2.DRAFT.md / COMPLETE_READS.V2.DRAFT.json','READ-INDEX.md / COMPLETE_READS.json')
    prompt=prompt.replace('Read every mandatory chunk', 'READ-REUSE-SUMMARY and REUSED-READS authenticate your own full prior model-visible reads of 26 unchanged documents (8,913 lines). They include unchanged launch, Guest and Hermit time context, original measurement logs and selected unchanged caller bodies. Their current bytes match exactly. Do not needlessly reread them; use them for adaptive follow-up as needed. This reuse does not borrow a peer or excuse reading any changed file. Read every mandatory chunk')
    start=prompt.index('Evidence status at preparation:')
    end=prompt.index('\n\nGOALPOST-MOVING REVIEW RULE',start)
    evidence=f'''Actual evidence: Q2 final target SHA256 0b4ab5639249a24e72c1249d255217552ae0147f1287b2036fb2d24f4de9d624 attempted 45 phases, accepted 44 and retained one refusal. Thirty-seven declarations qualified. The 38th, unchanged legacy-vsyscall test, returned zero with exact test/suite 1/0/0 success, but its original strict JSON reader refused the 26 interleaved ANSI tracing records. Do not call it qualified or all 45 phases accepted. All 38 raw outputs, complete 45-outcome projection, limits, retained fresh ELF/library records, actual clock observations and original receipts are bound. Complete actual RESULTS and all per-phase records are available for follow-up. Source bytes equal this Git head; no build or guest rerun occurred after commit.

The separate offline legacy interpretation and strict integer-count correction are optional author support, with complete reader/control sources bound for follow-up if needed. The mandatory actual raw stream and real legacy test/helper preserve all four exact libtest records, all 26 tracing lines and the time-interception pair; original accepted=false remains untouched. The earlier prototype's bool/float count-equality defect remains preserved. Decide explicitly whether the actual evidence supports the legacy behavior while retaining the original caller refusal. The 16 offline corruption probes are not product tests and do not change any denominator. This source review does not require another product run or a new general parser design.

The first clock-v2 metadata passed and compile failed raw101; all 38 tests were then unrun. The two-import successor corrected only those imports. Old actual [3,67] against required [1,65], the debugger wrapper raw125 and failed mapped-page reads remain failures. Task-bound read(fd) counts and successful step/restore evidence are narrower observations. The original measured launcher interval was 223 branches / 2,230 ns; older 2,270 ns belongs to a distinct historical run. Current exact observations are post_exec=0, guest boundaries=1/65, post-exec timer=1 and pre-loop boundaries=65/129. No fixed offset, tolerant comparator or filtering was introduced. Source/evidence summary: {F/'SOURCE-AND-EVIDENCE.md'}.

This remains finite component qualification, not a Hermit invocation, full backend parity or whole scheduler clearance. The existing precise-timer parent can return early and captures child output internally; parent success does not independently prove both child modes. Other declared unmeasured paths remain unmeasured. Require your WHOLE-PR verdict on {BASE}..{HEAD}, preserving those limits. Do not transfer your prior design or unrelated source approval.'''
    prompt=prompt[:start]+evidence+prompt[end:]
    req('[PENDING' not in prompt and 'PREPARATION ONLY' not in prompt,'unfinished prompt')
    prompt_name='prompt.txt' if lane=='claude' else '01-source.prompt.txt'
    put(out/prompt_name,prompt)
    put(out/'PROMPT-DELTA.patch',''.join(difflib.unified_diff(draft.splitlines(True),prompt.splitlines(True),fromfile=str(P/lane/('prompt.DRAFT.txt' if lane=='claude' else '01-source.prompt.DRAFT.txt')),tofile=str(out/prompt_name))))
    target=json.loads((P/lane/'TARGET.DRAFT.json').read_text())
    target.update(scope='Whole exact PR588 clock and syscall-injection source review; finite component evidence and explicit legacy refusal',head=HEAD,tree=TREE,pr=URL,**count,final_qualification_pending=False,executable_target_deliberately_absent=False,expected_symlinks=7,qualified_declarations=37,attempted_phases=45,accepted_phases=44,legacy_original_accepted=False,committed_proof=rec(F/'COMMITTED-SOURCE-PROOF.json'),qualification_target=rec(RESULT/'TARGET.json'),read_reuse=rec(out/'REUSED-READS.json'),grounding_authentication=rec(P/lane/'GROUNDING_AUTHENTICATION.json'))
    put(out/'TARGET.json',target);targets[lane]=rec(out/'TARGET.json')
    previous=json.loads((P/lane/'inputs.DRAFT.json').read_text());held={r['path']:r for r in previous['files']}
    def bind(path):
        r=rec(path);held[r['path']]=r
    for row in json.loads((out/'REUSED-READ-INPUTS.json').read_text())['files']:bind(row['path'])
    for row in json.loads((F/'Q2-EVIDENCE-INPUTS.json').read_text())['files']:bind(row['path'])
    for row in legacy_records:bind(row['path'])
    for path in [LEG/'TARGET.json',LEG/'REPORT.md',LEG/'READBACK.json',LEG/'SOURCE-BINDING.json',STRICT/'TARGET.json',STRICT/'READBACK.json',F/'COMMITTED-SOURCE-PROOF.json',F/'verify_committed_source.py',F/'Q2-EVIDENCE-READBACK.json',F/'Q2-EVIDENCE-INPUTS.json',F/'bind_qualification.py',F/'prepare_read_reuse.py',F/'finalize_packet.py']:
        bind(path)
    for r in json.loads((LEG/'SOURCE-BINDING.json').read_text())['files']:bind(r['path'])
    root_readback=RESULT/'ROOT-RESULT-READBACK.json'
    if root_readback.exists():bind(root_readback)
    root_source_readback=F/'ROOT-COMMITTED-SOURCE-READBACK.json'
    if root_source_readback.exists():bind(root_source_readback)
    for d in documents:
        bind(d['document']['path'])
        for c in d['chunks']:bind(c['path'])
    for path in out.iterdir():
        if path.is_file():bind(path)
    # Bind actual inputs, not mutable cache artifacts or the other review lane.
    for row in held.values():check(row)
    for row in previous['symlinks']:
        q=Path(row['path']);req(q.is_symlink() and os.readlink(q)==row['target'],'alias drift')
    put(out/'inputs.json',dict(files=sorted(held.values(),key=lambda r:r['path']),symlinks=previous['symlinks']))

put(F/'PLAN.md',f'''Root inspection is required before launching either prepared continuation for {URL}, {BASE}..{HEAD}, tree {TREE}.

Use the unchanged launcher controls from ../DISPATCH-PLAN.md: whole invocation through with-proxy, explicit outer PYTHONOPTIMIZE=0, each own session and no model override. Fresh all-UID cap-15 admission immediately before each launch; verify the first actual initialization/model/session/policy before taking the second fresh census. No launch occurred during preparation.

Concrete prospective commands:

    with-proxy env PYTHONOPTIMIZE=0 /usr/bin/python3 -B {F/'claude/launch-review.py'}
    with-proxy env PYTHONOPTIMIZE=0 /usr/bin/python3 -B {F/'native/launch-guard.py'}

Claude: 1,800-second wall, TERM then 10-second KILL, 64 MiB per raw output; plan permission, requested Read/Grep/Glob, expected actual claude-opus-5[1m], session 5d98403d-b8e9-42bf-8e2f-da7e7b70809c. Native: 1,800-second wall, TERM then three-second KILL, 64 MiB combined raw outputs; actual never/read-only and gpt-5.6-sol, session 01a0b38e-9e3c-7883-9816-65199c048935. Requested policy and available tools are not actual-use evidence; inspect actual init/context promptly. Keep all handles through termination and preserve all errors/refusals without automatic retry.

Grounding is each session's own ordered primary reading, exact current H97c scheduler and current vision bytes. Separately, 26 unchanged fully read documents /8,913 lines per lane are reauthenticated from own raw outputs and reused. All six changed files remain mandatory full reads, alongside actual Git diff, every new body, old causal-oracle delta, new outcome evidence and complete changed caller/parser material. No peer findings cross lanes. Legacy remains raw success plus original reader refusal; the separate strict offline interpretation and earlier count-type defect are visible.

Terminal audits: under PYTHONOPTIMIZE=0 run Claude audit-final.py; native audit_source.py with a fresh audit tag and, if required by actual bounded split reads, its existing audit_split_reads.py. Preserve incomplete first coverage instead of inventing credit. Audit native actual turn context and before/after input custody; coarse stage order is not a claim of full listed-source order. Preserve Claude assistant-text events and terminal result separately, extract the report by explicit event identity, and report actual tools. Full coverage/raw zero is not approval. Root reads genuine whole-PR verdicts and owns publication/landing.

No builds, tests, guest runs, leases, network writes, model override, source/SCM edit or external reviewer launch is authorized by this plan. Exact source, results and prompts must remain held through the review. Source changes require an explicit successor, not editing the running packet.
''')
put(F/'REPORT.md',f'''Final runnable review preparation for {URL} is frozen on {HEAD}, parent {BASE}, tree {TREE}. No reviewer has been launched. Root final inspection remains required.

Whole committed-source equality covers 2,632 entries, seven aliases, three unchanged gitlinks and the distinct ignored Cargo.lock. Q2 binds all 45 outcomes, retained fresh ELFs/libraries and unchanged bounds. Counts remain 44 accepted phases and 37 qualified declarations; legacy payload success / original reader refusal is explicit. Its separate offline interpretation, strict count-type correction and 16 negative transcript probes do not rewrite qualification or add product tests. The earlier compile raw 101 remains preserved.

Own-session complete read reuse removes 26 duplicate documents / 8,913 lines each. Current mandatory populations are Claude {stats['claude']['mandatory_documents']} documents / {stats['claude']['mandatory_chunks']} chunks / {stats['claude']['mandatory_lines']} lines, native {stats['native']['mandatory_documents']} / {stats['native']['mandatory_chunks']} / {stats['native']['mandatory_lines']}. These are new review reading populations, not test counts. Full changed files and required actual evidence remain mandatory. Exact prior reports and canonical grounding are separate, with no peer findings supplied.

All launch/helper bytes match the inspected draft helpers, with the earlier disclosed Claude scope-text change only. Prompt deltas record final head, real outcomes and authenticated read reuse. PLAN contains concrete commands and the unchanged cap-15, actual-policy,1,800-second/64-MiB controls. No source verdict or landing approval is claimed.
''')
setup=dict(pr=URL,head=HEAD,base=BASE,tree=TREE,report=rec(F/'REPORT.md'),plan=rec(F/'PLAN.md'),source_proof=rec(F/'COMMITTED-SOURCE-PROOF.json'),qualification=rec(RESULT/'TARGET.json'),evidence_readback=rec(F/'Q2-EVIDENCE-READBACK.json'),legacy=rec(LEG/'TARGET.json'),legacy_strict=rec(STRICT/'TARGET.json'),lanes={lane:dict(target=targets[lane],prompt=rec(F/lane/('prompt.txt' if lane=='claude' else '01-source.prompt.txt')),inputs=rec(F/lane/'inputs.json'),coverage=stats[lane]) for lane in ['claude','native']},external_launches=0,root_authorization_pending=True)
put(F/'SETUP.json',setup)
put(F/'READBACK.json',dict(setup=rec(F/'SETUP.json'),report=rec(F/'REPORT.md'),plan=rec(F/'PLAN.md'),no_launch=True,source_approval=False,live_product_unchanged_by_preparer=True,source_qualification_counts=dict(attempted=45,accepted=44,qualified_declarations=37,legacy_original_accepted=False)))
print(json.dumps(dict(setup=rec(F/'SETUP.json'),readback=rec(F/'READBACK.json'),coverage=stats),indent=2))
