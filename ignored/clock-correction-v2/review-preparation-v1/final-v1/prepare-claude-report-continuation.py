from pathlib import Path
import json,hashlib,stat,sys
F=Path(__file__).resolve().parent;C=F/'claude-coverage-continuation-v1';O=F/'claude';P=F/'claude-report-continuation-v1'
def need(v,m):
 if not v:raise RuntimeError(m)
def load(p):return json.loads(Path(p).read_text())
def rec(p):
 p=Path(p);b=p.read_bytes();return {'path':str(p),'bytes':len(b),'sha256':hashlib.sha256(b).hexdigest(),'mode':stat.S_IMODE(p.stat().st_mode)}
def put(n,s):
 with (P/n).open('x') as f:f.write(s)
def jput(n,v):put(n,json.dumps(v,indent=2)+'\n')
need(sys.flags.optimize==0,'optimization');P.mkdir()
t=load(C/'TARGET.json');timeout=load(C/'TIMEOUT-READBACK.json');need(timeout['raw_exit_code']==124 and timeout['literal_read_union']=={'original_visible':224,'new_raw_visible':16,'original_required':240,'new_rule_visible':1},'preserved evidence')
goal='''GOALPOST-MOVING REVIEW RULE

Adversarial reviewers must look explicitly for goalpost moving. We are extremely skeptical of any goalpost moving. YOU DO NOT CLEAR THE BAR BY SIMPLY LOWERING THE BAR.

Treat each of these as an explicit review target:
- weakening an assertion so a test passes
- widening a tolerance · adding an exemption · skipping a case · relaxing a comparator
- renaming or relabelling so a failure reads as a pass
- deleting a check rather than satisfying it'''
rule=(C/'TRIGGER-RULE.txt').read_text()
prompt=f'''Continue your own existing independent review session {t['session_id']} for https://github.com/rrnewton/reverie/pull/588. This invocation asks only for the missing supplemental report, approximately 1,000 words if sufficient. No source changed. Do not repeat complete source or raw-chunk reading merely to restate it.

Whole target: {t['base']}..{t['head']}, tree {t['tree']}.

Your first report ({O/'assistant-event-562-block-0.txt'}, SHA256 5888d24a142e316518fe16cc07ba2e33b8f76f2aab8e30ca3b94f309aed7ce7b) gave a whole-source APPROVE but simultaneously disclosed only 8/24 raw trace chunks read. The original mandatory audit failed: 224/240 chunks. Its opening mandatory-reading completion claim was inaccurate. The report and failed audit remain unchanged.

Your immediately preceding 600-second continuation read all sixteen missing raw chunks and the exact trigger-rule excerpt completely, with 19 actual Read calls and no tool error. It emitted seven compacting statuses then a null status, resumed reading and reached the final required excerpt. All seventeen new reads are proven by exact model-visible line matching, but the guard expired at 600.055 seconds (raw 124) before a supplemental verdict. The failed terminal audit and timeout remain failures. No source or input changed.

The reading union is now complete across distinct invocations: original 224 + supplemental 16 = all original 240 mandatory chunks, including every changed source file. The extra rule excerpt was also read. This is evidence of reading, not a substitute for YOUR source verdict. Exact union/custody are {C/'TIMEOUT-COVERAGE.json'} (SHA256 ad48ff80606bc02019a0cce2c766f53c63883e7f59b827e84297594253135a0b) and {C/'TIMEOUT-READBACK.json'} (SHA256 32a71b7e10979f2b1b42db88c53cc6351f0be1caa81fed267f38d4d7eb28e22e). Reuse your own authenticated canonical paper/full scheduler/vision/roadmap grounding and your own completed source/evidence reading. There are no mandatory re-reads in this report-only invocation. If you need a directly relevant clarification, only read bound sources. Do not execute anything, mutate files, use network tools, delegate or consult another reviewer's material.

Please now issue a concise self-contained supplemental report:
1. Correct the first report's completion claim explicitly. State distinct original 224/240 plus supplemental 16/16, preserve the first failure and subsequent timeout, and do not claim initial strict read order or a successful second invocation.
2. State whether the newly read raw records change any source finding, arithmetic or evidence conclusion. Give concrete source/event pointers for any change. Reassess your own A1–A5 as needed; do not reproduce the entire previous report.
3. Independently reassess trigger 2 against the actual rule below. Your prior public-shape-only reasoning omitted its syscall-interception-model clause. State your conclusion on the actual pending-original ownership/injection changes and trigger 3, with a short reason. No other reviewer's conclusion is supplied.
4. Give an unambiguous whole-exact-head APPROVE / CHANGES REQUESTED / UNABLE TO COMPLETE verdict and material limits. This must be your judgment; neither a desired answer nor new product evidence is being supplied.

Exact existing Hermit AGENTS.md lines 452–460, already read:
{rule}
Preserve actual qualification: 45 phases attempted, 44 accepted, 37 declarations qualified. Legacy payload raw 0 and actual assertions do not replace its accepted=false structured-reader receipt. Preserve earlier compile 101, unchanged [1,65] baseline failure with actual [3,67], diagnostic raw 125, precise-timer child-record limits and all unmeasured states. No new Hermit run, parity, scheduler or activation approval follows.

Only your own original findings and readings are supplied. The source, tests, assertions, caller, bounds and prior records remain unchanged. If unable to finish under this separate 600-second bound, say so; do not infer a verdict from completed reading.

{goal}
'''
put('prompt.txt',prompt)
for name in ['launch-review.py','audit-final.py']:put(name,(C/name).read_text())
jput('COMPLETE_READS.json',[])
target={**t,'scope':'Report-only supplemental whole exact PR588 verdict after authenticated distinct 224+16 reading; both prior failures preserved','stage':'03-report','mandatory_documents':0,'mandatory_chunks':0,'mandatory_lines':0,'new_raw_chunks_required':0,'reading_union':rec(C/'TIMEOUT-COVERAGE.json'),'preceding_timeout':rec(C/'TIMEOUT-READBACK.json'),'previous_refusal':rec(O/'audit-final.status.json'),'report_only':True}
jput('TARGET.json',target)
put('PLAN.md',f'''One proposed same-session report-only continuation. No launch until coordinator authorization.

The original whole-source report and failed 224/240 audit remain frozen. The next invocation completed sixteen missing raw reads plus the rule paragraph but timed out raw 124 before its verdict. Exact 224+16 union is separately authenticated. No new source read is mandatory here because the original reading set is already complete; this does not waive a missing read. This invocation obtains the reviewer's own short reassessment/correction and whole-head verdict.

Command:
with-proxy env PYTHONOPTIMIZE=0 /usr/bin/python3 -B {P/'launch-review.py'}

Both launcher and terminal audit are byte-identical to the preceding inspected 600-second caller. Same 600-second TERM/10-second KILL, 64 MiB per-file caps, fresh all-UID cap-15/no-concurrent-resume checks, same session/model, actual plan initialization inspection, Read/Grep/Glob only, no model override, whole with-proxy, held inputs and aliases, explicit outer optimize=0. Only prompt/scope/dimensions and new immutable evidence/output paths change. All prior source/evidence/helper bindings remain held.

The unchanged terminal audit checks raw zero, before/after bytes, actual session/model/plan/tools and preserves every assistant event plus the terminal result. It has zero new mandatory chunks because the exact original 240 were consumed across the two prior invocations. A separate final custody record must bind that union and retain both the original coverage refusal and timeout. Actual source approval still comes only from the substantive supplemental report. Stop on refusal; do not retry or extend limits.
''')
jput('HELPER-CONTINUITY.json',{'launch':{'before':rec(C/'launch-review.py'),'after':rec(P/'launch-review.py'),'identical':(C/'launch-review.py').read_bytes()==(P/'launch-review.py').read_bytes()},'audit':{'before':rec(C/'audit-final.py'),'after':rec(P/'audit-final.py'),'identical':(C/'audit-final.py').read_bytes()==(P/'audit-final.py').read_bytes()}})
oldinputs=load(C/'inputs.json');files={r['path']:r for r in oldinputs['files']}
more=[C/n for n in ['SETUP.json','exit.json','launch.json','capacity-before.json','INITIAL-POLICY-READBACK.json','source-before.json','source-after.json','output.jsonl','stderr.log','audit-final.status.json','audit-final.stdout','audit-final.stderr','TIMEOUT-READBACK.json','TIMEOUT-COVERAGE.json','TIMEOUT-ASSISTANT-TEXTS.json','COMPACTION-EVENTS.json','freeze-timeout.py']]+[Path(__file__)]+list(P.iterdir())
for q in more:files[str(q)]=rec(q)
jput('inputs.json',{'files':sorted(files.values(),key=lambda r:r['path']),'symlinks':oldinputs['symlinks']})
jput('SETUP.json',{'scope':'Prepared report-only continuation, no new source or execution machinery','head':t['head'],'session':t['session_id'],'files':len(files),'aliases':len(oldinputs['symlinks']),'new_required_reads':0,'existing_required_read_union':240,'wall_seconds':600,'per_output_file_bytes':67108864,'launch_authorized':False,'command':['with-proxy','env','PYTHONOPTIMIZE=0','/usr/bin/python3','-B',str(P/'launch-review.py')],'references':[rec(P/n) for n in ['TARGET.json','PLAN.md','prompt.txt','COMPLETE_READS.json','HELPER-CONTINUITY.json','inputs.json','launch-review.py','audit-final.py']]})
print(json.dumps(rec(P/'SETUP.json')));print(json.dumps({'files':len(files),'aliases':len(oldinputs['symlinks']),'path':str(P)}))
