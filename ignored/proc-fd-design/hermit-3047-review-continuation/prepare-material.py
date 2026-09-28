from pathlib import Path
import json,hashlib
p=Path(__file__).resolve().parent
m=json.loads((p/'manifest.json').read_text());events=json.loads((p/'prior-public-transcript.json').read_text())
def put(name,b):
 q=p/name;q.parent.mkdir(exist_ok=True,parents=True)
 with q.open('xb') as f:f.write(b)
 m['files'].append({'path':str(q),'bytes':len(b),'sha256':hashlib.sha256(b).hexdigest(),'mode':'0o644'})
 return {'path':name,'bytes':len(b),'sha256':hashlib.sha256(b).hexdigest()}
requests={x['id']:x for x in events if x['kind']=='tool_request'}
blocks=[]
for e in events:
 if e['kind']=='assistant_public_text':blocks.append('PUBLIC ASSISTANT TEXT (not a verdict)\n'+e['text']+'\n\n')
 if e['kind']=='tool_result':
  q=requests[e['tool_use_id']]
  body=e['content'] if isinstance(e['content'],str) else json.dumps(e['content'],ensure_ascii=False,indent=2)
  blocks.append('SUCCESSFUL PUBLIC TOOL REQUEST\n'+json.dumps(q,ensure_ascii=False,indent=2)+'\nPUBLIC TOOL RESULT (exact text)\n'+body+'\n\n')
parts=[];current='';numbers=[]
for i,block in enumerate(blocks,1):
 assert len(block.encode())<=60000
 if current and len((current+block).encode())>60000:
  name=f'public-material/part-{len(parts)+1:02d}.txt';parts.append({**put(name,current.encode()),'public_blocks':numbers});current='';numbers=[]
 current+=block;numbers.append(i)
if current:parts.append({**put(f'public-material/part-{len(parts)+1:02d}.txt',current.encode()),'public_blocks':numbers})
put('public-material/parts.json',json.dumps(parts,indent=2).encode()+b'\n')
put('PRIOR-MATERIAL-INDEX.md',('The prior timed-out review returned no verdict. Use these bounded parts of its successful public source/evidence reads to continue the analysis without repeating all original reads. They preserve every successful public result once in order, along with its original request. No hidden thinking or signatures are present. The exact canonical public transcript and individual sidecars remain available for audit; do not reread their duplicate representations as extra evidence.\n\n'+''.join(f'- {x["path"]}: {x["bytes"]} bytes; public blocks {x["public_blocks"][0]}–{x["public_blocks"][-1]}; SHA256 {x["sha256"]}.\n' for x in parts)+'\nOne unsuccessful prior Grep requested detcore/src/consts.rs, which was missing from that partial copied context. It is now provided from exact a9 at source/hermit/detcore/src/consts.rs; the old failed request is recorded in missing-context.json. No product source changed.\n').encode())
put('missing-context.json',json.dumps({'failed_tool':'Grep','requested_path':'prior source/hermit/detcore/src/consts.rs','error':'Path did not exist in the partial review snapshot','correction':'Added immutable a9 source/hermit/detcore/src/consts.rs as context; no production mutation','failed_result_omitted_from_success_transcript':True},indent=2).encode()+b'\n')
(p/'manifest.json').write_text(json.dumps(m,indent=2)+'\n')
print(json.dumps({'material_parts':parts,'total_bytes':sum(x['bytes'] for x in parts)},indent=2))
