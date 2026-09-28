"""Authenticate complete prior reads from each actual reviewer's own raw outputs."""
from pathlib import Path
from collections import Counter
import hashlib
import json
import re
import stat
import sys

F = Path(__file__).resolve().parent
P = F.parent
L = P.parents[2]
D = Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-replay-prerequisites-20260918/ignored/kvm-clock-design-review-preparation-v1')
PRIOR = L/'ignored/parity-composition-v1/review-preparation-v1/final-v1'
SID={'claude':'5d98403d-b8e9-42bf-8e2f-da7e7b70809c','native':'01a0b38e-9e3c-7883-9816-65199c048935'}

def req(ok, why):
    if not ok: raise RuntimeError(why)

def rec(path):
    p=Path(path);req(p.is_file() and not p.is_symlink(),str(p));h=hashlib.sha256()
    with p.open('rb') as stream:
        for data in iter(lambda:stream.read(1024*1024),b''):h.update(data)
    return dict(path=str(p),bytes=p.stat().st_size,sha256=h.hexdigest(),mode=stat.S_IMODE(p.stat().st_mode))

def check(row):
    actual=rec(row['path']);req(all(actual[k]==row[k] for k in ['bytes','sha256']),str(row['path']));return actual

def put(path,value):
    Path(path).parent.mkdir(parents=True,exist_ok=True)
    with Path(path).open('x') as stream:json.dump(value,stream,indent=2);stream.write('\n')

def index(path):
    data=json.loads(path.read_text());return data if isinstance(data,list) else data['documents']

def claude_reads(base, repair=None):
    sources=[base/'output.jsonl']+([repair/'output.jsonl'] if repair else [])
    calls={};results={};inputs=[]
    for source in sources:
        inputs.append(rec(source));events=[json.loads(line) for line in source.read_text().splitlines() if line.strip()]
        init=[e for e in events if e.get('type')=='system' and e.get('subtype')=='init']
        req(len(init)==1 and init[0].get('session_id')==SID['claude'],'wrong Claude session')
        for ei,e in enumerate(events):
            for b in e.get('message',{}).get('content',[]):
                if not isinstance(b,dict):continue
                if b.get('type')=='tool_use':
                    req(b['id'] not in calls,'duplicate call id');calls[b['id']]=(b,str(source),ei)
                elif b.get('type')=='tool_result':
                    req(b['tool_use_id'] not in results,'duplicate result');results[b['tool_use_id']]=(b,str(source),ei)
    def prove(chunk):
        expected=Path(chunk['path']).read_text().splitlines();covered=set();matches=[]
        for key,(call,source,ei) in calls.items():
            if call.get('name')!='Read' or call.get('input',{}).get('file_path')!=chunk['path']:continue
            result=results.get(key)
            if not result or result[0].get('is_error'):continue
            content=result[0].get('content');req(isinstance(content,str),'unexpected Read output type')
            visible={int(m.group(1)):m.group(2) for line in content.splitlines() if (m:=re.match(r'^(\d+)\t(.*)$',line))}
            lines=[n for n,line in enumerate(expected,1) if visible.get(n)==line]
            if lines:
                covered.update(lines);matches.append(dict(call_id=key,raw_path=source,call_event=ei,result_event=result[2],first_visible=min(lines),last_visible=max(lines),matching_lines=len(lines)))
        return dict(complete=len(covered)==len(expected),physical_lines=len(expected),matches=matches)
    return prove,inputs

def native_reads(base, stage):
    raw=base/(stage+'.final-MODEL-TOOLS.json');rows=json.loads(raw.read_text())
    result=json.loads((base/(stage+'.result.json')).read_text());req(result['session_resume']==SID['native'] and result['raw_exit_code']==0,'wrong native session or nonterminal result')
    calls=Counter(x['payload']['call_id'] for x in rows if x['payload']['type'] in ('custom_tool_call','function_call'))
    outputs=[x for x in rows if x['payload']['type'].endswith('_output')]
    counts=Counter(x['payload']['call_id'] for x in outputs)
    def text(row):
        value=row['payload'].get('output','');return value if isinstance(value,str) else '\n'.join(x.get('text','') for x in value if isinstance(x,dict))
    def prove(chunk):
        expected=Path(chunk['path']).read_text();matches=[]
        for row in outputs:
            cid=row['payload']['call_id']
            if calls[cid]==1 and counts[cid]==1 and (expected in text(row) or expected.rstrip('\n') in text(row)):
                matches.append(dict(call_id=cid,raw_path=str(raw),persisted_line=row['line'],timestamp=row['timestamp']))
        return dict(complete=bool(matches),physical_lines=len(expected.splitlines()),matches=matches)
    return prove,[rec(raw),rec(base/(stage+'.result.json')),rec(base/(stage+'.final-COVERAGE.json'))]

req(sys.flags.optimize==0,'optimization must be disabled')
summary={}
for lane in ['claude','native']:
    proofs=[];held={}
    for root,stage in [(D,'01-design'),(PRIOR,'01-source')]:
        base=root/lane
        prove,raws=claude_reads(base,D/'claude-coverage-v2' if root==D else None) if lane=='claude' else native_reads(base,stage)
        for r in raws:held[r['path']]=r
        ipath=base/'COMPLETE_READS.json';held[str(ipath)]=rec(ipath)
        for doc in index(ipath):
            source=doc.get('source',doc.get('document'));check(source)
            assembled='';pieces=[]
            for c in doc['chunks']:
                check(c);assembled+=Path(c['path']).read_text();pieces.append(dict(chunk=c,**prove(c)))
            req(assembled==Path(source['path']).read_text(),'old chunk assembly mismatch')
            proofs.append(dict(source=source,prior_index=rec(ipath),complete=all(c['complete'] for c in pieces),chunks=pieces))
    by_hash={}
    for proof in proofs:
        if proof['complete']:by_hash.setdefault(proof['source']['sha256'],proof)
    reused=[];must_read=[]
    draft=P/lane/'COMPLETE_READS.V2.DRAFT.json'
    for doc in index(draft):
        source=doc.get('source',doc.get('document'));check(source)
        proof=by_hash.get(source['sha256'])
        if proof:
            req(Path(source['path']).read_bytes()==Path(proof['source']['path']).read_bytes(),'hash equality without byte equality')
            for c in proof['chunks']:held[c['chunk']['path']]=check(c['chunk'])
            held[proof['source']['path']]=check(proof['source'])
            reused.append(dict(name=doc['name'],current_source=source,prior_source=proof['source'],lines=len(Path(source['path']).read_text().splitlines()),proof=proof))
        else:must_read.append(doc)
    destination=F/lane
    put(destination/'REUSED-READS.json',dict(session_id=SID[lane],source_reuse_only=True,canonical_grounding_separate=True,python_optimize=sys.flags.optimize,reused_documents=len(reused),reused_lines=sum(x['lines'] for x in reused),documents=reused,limitations='Own full literal model-visible reads and exact current bytes only. No peer report or verdict is reused; no claim of new reading, listed source order, or current source approval. Original Claude 155/163 failure and distinct eight-chunk repair remain separate.'))
    put(destination/'REUSED-READ-INPUTS.json',dict(files=sorted(held.values(),key=lambda r:r['path'])))
    put(destination/'UNREAD-SOURCE-DOCUMENTS.json',dict(documents=must_read,qualification_documents_to_add=True))
    summary[lane]=dict(reused_documents=len(reused),reused_lines=sum(x['lines'] for x in reused),new_source_documents=len(must_read),new_source_chunks=sum(len(x['chunks']) for x in must_read),proof=rec(destination/'REUSED-READS.json'))
put(F/'READ-REUSE-SUMMARY.json',summary)
print(json.dumps(summary,indent=2))
