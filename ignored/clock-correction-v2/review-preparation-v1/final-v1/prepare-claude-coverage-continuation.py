from pathlib import Path
import json,hashlib,stat,difflib,sys
F=Path(__file__).resolve().parent;O=F/'claude';P=F/'claude-coverage-continuation-v1'
def require(x,m):
 if not x:raise RuntimeError(m)
def rec(p):
 p=Path(p);b=p.read_bytes();return {'path':str(p),'bytes':len(b),'sha256':hashlib.sha256(b).hexdigest(),'mode':stat.S_IMODE(p.stat().st_mode)}
def put(n,s):
 with (P/n).open('x') as f:f.write(s)
def jput(n,v):put(n,json.dumps(v,indent=2)+'\n')
def load(p):return json.loads(Path(p).read_text())
require(sys.flags.optimize==0,'optimization');P.mkdir()
old=load(O/'TARGET.json');diag=load(O/'INCOMPLETE-COVERAGE-DIAGNOSTIC.json');missing=diag['missing_chunks'];require(len(missing)==16 and diag['covered_chunks']==224,'expected missing set')
require(all(m['document']=='entry-replay-raw-events' and not m['complete_read_tool_ids'] for m in missing),'only raw trace gaps')
parts=[m['file'] for m in missing];raw=''.join(Path(r['path']).read_text() for r in parts)
put('MISSING-RAW-EVENTS.jsonl',raw)
ag=F.parent/'common/hermit-97c/AGENTS.md';lines=ag.read_text().splitlines(keepends=True);excerpt=''.join(lines[451:460]);require('syscall-interception model' in excerpt,'protocol excerpt');put('TRIGGER-RULE.txt',excerpt)
jput('MISSING-CHUNKS.json',{'original_target':rec(O/'TARGET.json'),'original_report':rec(O/'assistant-event-562-block-0.txt'),'original_failed_audit':rec(O/'audit-final.status.json'),'original_audit_error':rec(O/'audit-final.stderr'),'original_complete_chunks':224,'original_required_chunks':240,'missing':parts,'concatenation':rec(P/'MISSING-RAW-EVENTS.jsonl'),'trigger_original_source':rec(ag),'trigger_first_line':452,'trigger_last_line':460,'trigger_excerpt':rec(P/'TRIGGER-RULE.txt')})
complete=[{'name':'missing-entry-replay-raw-events','source':rec(P/'MISSING-RAW-EVENTS.jsonl'),'chunks':parts},{'name':'exact-existing-trigger-rule','source':rec(P/'TRIGGER-RULE.txt'),'chunks':[dict(rec(P/'TRIGGER-RULE.txt'),first_line=1,last_line=len(excerpt.splitlines()))]}]
jput('COMPLETE_READS.json',complete)
index=['Read each of these existing raw chunk files in full, in the listed order. These are precisely the sixteen not read in the first invocation. Do not substitute TRACE-ANALYSIS.json or sample windows.','']
for i,r in enumerate(parts,1):index.append(f"{i}. {r['path']} ({r['bytes']} bytes; SHA256 {r['sha256']})")
index+=['',f"17. {P/'TRIGGER-RULE.txt'} (exact already-bound Hermit AGENTS.md lines 452–460)",'']
put('READ-INDEX.md','\n'.join(index))
goal='''GOALPOST-MOVING REVIEW RULE

Adversarial reviewers must look explicitly for goalpost moving. We are extremely skeptical of any goalpost moving. YOU DO NOT CLEAR THE BAR BY SIMPLY LOWERING THE BAR.

Treat each of these as an explicit review target:
- weakening an assertion so a test passes
- widening a tolerance · adding an exemption · skipping a case · relaxing a comparator
- renaming or relabelling so a failure reads as a pass
- deleting a check rather than satisfying it'''
prompt=f'''Continue ONLY your own existing Claude session {old['session_id']} as the independent reviewer of https://github.com/rrnewton/reverie/pull/588.

Exact whole target remains {old['base']}..{old['head']}, tree {old['tree']}. No product, test, source, oracle, qualification result or comparator changed. This is a bounded repair of missing mandatory evidence reading, not an author instruction to approve. Your full original report and failed audit are preserved unchanged.

First read {P/'READ-INDEX.md'} and read all sixteen listed raw-event chunks in full, then the exact trigger-rule excerpt. A machine audit found 224/240 original chunks completely visible, including every changed source file and all original required material except these sixteen chunks. Your original report correctly disclosed reading only 8/24 raw entry-replay chunks; its opening statement “I have completed the mandatory reading” was therefore false. Explicitly correct that statement. Do not claim the original invocation had complete coverage or strict index ordering. Reading now can complete a distinct supplemental set, not retroactively alter the failed audit.

Your own original full report is {O/'assistant-event-562-block-0.txt'} (SHA256 5888d24a142e316518fe16cc07ba2e33b8f76f2aab8e30ca3b94f309aed7ce7b). Original refusal is {O/'audit-final.stderr'}. Exact gap/input derivation is {P/'MISSING-CHUNKS.json'}. Reuse your own authenticated prior paper/full scheduler/product vision/roadmap grounding and your already-complete source reads; do not repeat them unnecessarily. All unchanged inputs remain held, with their identities in {P/'inputs.json'}. No other reviewer’s report or finding is provided or may be consulted.

After every missing raw chunk is read completely, reconsider your causal arithmetic, source findings A1–A5, evidence scope and verdict on the WHOLE exact base-to-head change. The remaining records may corroborate or refute your inference: reason from their actual content. Follow any directly necessary source question only in the bound sources, using read-only tools; identify unread material precisely. Do not execute a program, build, test, network command, delegation or mutation.

Also independently reassess trigger 2 against the actual already-read rule, whose precise text is:

{excerpt}
Your report's reasoning based only on public shape and unchanged Tool/Guest files is narrower than that rule. Assess whether the actual pending-original ownership/injection changes alter the syscall-interception model. Give your own conclusion with source justification. This is a classification question; no other reviewer’s answer is supplied.

Preserve the true finite evidence: 45 phases attempted, 44 accepted, 37 declarations qualified; the unchanged legacy declaration had payload raw 0 and real assertions but its original structured-reader receipt remains accepted=false. Preserve the compiler raw 101 predecessor, [3,67] versus unchanged [1,65] failure, raw 125 diagnostic refusal and all unmeasured states. Do not inflate the precise-timer parent's two internally captured child modes. No Hermit integration, full backend parity or whole-scheduler approval follows.

Return a self-contained supplemental report with: exact target; explicit correction of the original reading claim; each required raw chunk actually read and any residual gap; any changed/confirmed source findings with concrete pointers; trigger 2 and 3 classification; and an unambiguous whole-PR APPROVE / CHANGES REQUESTED / UNABLE TO COMPLETE verdict. Keep the first report historically intact. If the 600-second bound or any tool limit prevents complete reading, say so rather than issuing an unqualified completion claim.

{goal}
'''
put('prompt.txt',prompt)
launcher=(O/'launch-review.py').read_text();fixed=launcher.replace("'1800s'","'600s'").replace("'wall_seconds':1800","'wall_seconds':600")
require(fixed!=launcher and launcher.count("'1800s'")==1 and launcher.count("'wall_seconds':1800")==1,'expected two guard replacements')
put('launch-review.py',fixed);put('audit-final.py',(O/'audit-final.py').read_text())
put('LAUNCHER-DELTA.patch',''.join(difflib.unified_diff(launcher.splitlines(True),fixed.splitlines(True),fromfile=str(O/'launch-review.py'),tofile=str(P/'launch-review.py'))))
target={**old,'scope':'Supplemental missing-evidence coverage repair and whole exact PR588 verdict; original refusal preserved','stage':'02-coverage','mandatory_documents':2,'mandatory_chunks':17,'mandatory_lines':sum(len(Path(g['source']['path']).read_text().splitlines()) for g in complete),'original_report':rec(O/'assistant-event-562-block-0.txt'),'original_required_chunks':240,'original_complete_chunks':224,'new_raw_chunks_required':16,'wall_seconds':600}
jput('TARGET.json',target)
plan=f'''Prepare one supplemental same-session review. No launch is authorized by this packet itself.

The exact source/head and original qualification stay unchanged. Sixteen missing raw chunks plus the exact trigger-rule paragraph are the only required new reads. The original 224 complete chunks and all changed source reads retain their own actual-output provenance. Original failed audit and incomplete report remain untouched; the combined audit must say 224 + 16 rather than rewriting history as an initial complete pass.

Command after coordinator authorization:
with-proxy env PYTHONOPTIMIZE=0 /usr/bin/python3 -B {P/'launch-review.py'}

The launcher is the previously inspected helper with exactly two literal changes: timeout 1800s to 600s and recorded wall_seconds 1800 to 600. It retains TERM then 10-second KILL, 64 MiB per-output-file RLIMIT_FSIZE, explicit plan permission, requested Read/Grep/Glob tools, exact existing session, no model override, entire CLI invocation with-proxy, before/after input authentication, fresh all-UID cap-15 census and no parallel resume. Outer Python optimize=0 is explicit. Current expected model is claude-opus-5[1m]; inspect actual init/model/session/plan before inferring conformance. Availability and actual tool use are distinct.

The frozen audit-final.py is byte-identical to the original. It will check all 17 new required reads literally, every held input/alias, terminal status and actual tools. A separate append-only union readback will then compare these sixteen original paths/hashes against the original missing set, require all original 240 chunks complete across distinct invocations, and preserve the first audit failure/order limitations. No audit relaxation, reviewer model change, source change, product run or external write is requested. Stop on capacity, policy, input, timeout or coverage refusal; do not retry automatically.
'''
put('PLAN.md',plan)
oldinputs=load(O/'inputs.json');files={r['path']:r for r in oldinputs['files']}
extra=[O/'assistant-event-562-block-0.txt',O/'ASSISTANT-TEXT-EXTRACTION.json',O/'TERMINAL-RESULT-RAW.json',O/'output.jsonl',O/'exit.json',O/'source-before.json',O/'source-after.json',O/'audit-final.status.json',O/'audit-final.stderr',O/'audit-final.stdout',O/'INCOMPLETE-COVERAGE-DIAGNOSTIC.json',Path(__file__)]
extra+=list(P.iterdir())
for q in extra:
 require(q.is_file(),'unexpected input');files[str(q)]=rec(q)
jput('inputs.json',{'files':sorted(files.values(),key=lambda r:r['path']),'symlinks':oldinputs['symlinks']})
setup={'scope':'Prepared only; one bounded same-Claude missing-evidence continuation','head':old['head'],'files':len(files),'aliases':len(oldinputs['symlinks']),'mandatory_raw_chunks':16,'mandatory_rule_chunks':1,'source_unchanged':True,'launcher_only_delta':'Two 1800-to-600 guard/record literals; audit unchanged','command':['with-proxy','env','PYTHONOPTIMIZE=0','/usr/bin/python3','-B',str(P/'launch-review.py')],'references':[rec(P/n) for n in ['TARGET.json','PLAN.md','prompt.txt','READ-INDEX.md','MISSING-CHUNKS.json','COMPLETE_READS.json','inputs.json','launch-review.py','LAUNCHER-DELTA.patch','audit-final.py']]}
jput('SETUP.json',setup)
print(json.dumps(rec(P/'SETUP.json')));print(json.dumps({'path':str(P),'files':len(files),'aliases':len(oldinputs['symlinks']),'mandatory_lines':target['mandatory_lines']}))
