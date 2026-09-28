"""Append literal coverage for direct bounded sed reads; never infer from a filename alone."""
import collections,hashlib,json,pathlib,re,shlex,sys
P=pathlib.Path;O=P(__file__).resolve().parent;tag=sys.argv[1]
proof=json.loads((O/f'01-source.{tag}-COVERAGE.json').read_text());commands=json.loads((O/f'01-source.{tag}-COMMANDS.json').read_text());events=json.loads((O/f'01-source.{tag}-MODEL-TOOLS.json').read_text())
calls=collections.Counter(e['payload']['call_id'] for e in events if e['payload']['type'] in ('function_call','custom_tool_call'));outs=collections.Counter(e['payload']['call_id'] for e in events if e['payload']['type'].endswith('_output'))
def output(e):
 x=e['payload'].get('output','');return x if isinstance(x,str) else '\n'.join(i.get('text','') for i in x if isinstance(i,dict))
def rec(p):
 b=p.read_bytes();return dict(path=str(p),bytes=len(b),sha256=hashlib.sha256(b).hexdigest(),mode=p.stat().st_mode&0o7777)
extra=[]
for doc in proof['documents']:
 for c in doc['chunks']:
  if c['covered']:continue
  p=P(c['path']);lines=p.read_text().splitlines(keepends=True);ranges=[];mask=[False]*len(lines)
  for command in commands:
   if command.get('exit_code')!=0:continue
   shell=shlex.split(command['command']);inner=shlex.split(shell[-1])
   if len(inner)!=4 or inner[:2]!=['sed','-n']:continue
   m=re.fullmatch(r'(\d+),(\d+)p',inner[2])
   if not m:continue
   path=P(inner[3]);path=path if path.is_absolute() else O/path
   if path!=p:continue
   lo,hi=map(int,m.groups());literal=''.join(lines[lo-1:hi]);hits=[]
   if not literal:continue
   for e in events:
    pp=e['payload'];cid=pp['call_id']
    if pp['type'].endswith('_output') and calls[cid]==1 and outs[cid]==1 and (literal in output(e) or literal.rstrip('\n') in output(e)):
     hits.append(dict(call_id=cid,line=e['line'],timestamp=e['timestamp']))
   if hits:
    for i in range(lo-1,min(hi,len(lines))):mask[i]=True
    ranges.append(dict(first_line=lo,last_line=hi,command_id=command['id'],literal_sha256=hashlib.sha256(literal.encode()).hexdigest(),matches=hits))
  if ranges:extra.append(dict(chunk=rec(p),physical_lines=len(lines),complete=all(mask),read_ranges=ranges))
count=sum(x['complete'] for x in extra);result=dict(base_coverage=rec(O/f'01-source.{tag}-COVERAGE.json'),single_output_chunks=proof['covered_chunks'],additional_complete_chunks=count,combined_covered_chunks=proof['covered_chunks']+count,mandatory_chunks=proof['mandatory_chunks'],complete=proof['covered_chunks']+count==proof['mandatory_chunks'],split_reads=extra,scope='Only literal bytes from unique ordinary persisted outputs count. Ordered scheduler grounding remains separately authenticated; source read order is not a new grounding claim.')
p=O/f'01-source.{tag}-SPLIT-READ-COVERAGE.json'
with p.open('x') as f:json.dump(result,f,indent=2);f.write('\n')
print(json.dumps(result,indent=2))
