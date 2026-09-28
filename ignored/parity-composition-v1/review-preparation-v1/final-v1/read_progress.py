from pathlib import Path
import collections,datetime,json
P=Path(__file__).resolve().parent
report={}
for lane in ['claude','native']:
 d=P/lane;raw=d/('output.jsonl'if lane=='claude'else'01-source.jsonl')
 if not raw.exists():continue
 assert raw.stat().st_size<=64*1024*1024
 events=[]
 for line in raw.read_bytes().splitlines():
  try:events.append(json.loads(line))
  except json.JSONDecodeError:pass
 if lane=='claude':
  calls=[];errors=[];texts=[]
  for e in events:
   for b in e.get('message',{}).get('content',[]):
    if not isinstance(b,dict):continue
    if b.get('type')=='tool_use':calls.append(dict(id=b['id'],name=b['name'],input=b.get('input')))
    if b.get('type')=='tool_result'and b.get('is_error'):errors.append(b)
    if b.get('type')=='text'and e.get('type')=='assistant':texts.append(b['text'])
  value=dict(bytes=raw.stat().st_size,tool_calls=len(calls),tools=dict(collections.Counter(c['name']for c in calls)),errors=errors,last_calls=calls[-2:],latest_assistant=texts[-1][-7000:]if texts else None,terminal=(d/'exit.json').exists())
 else:
  commands=[e['item']for e in events if e.get('type')=='item.completed'and e.get('item',{}).get('type')=='command_execution'];texts=[e['item']['text']for e in events if e.get('type')=='item.completed'and e.get('item',{}).get('type')=='agent_message']
  value=dict(bytes=raw.stat().st_size,completed_commands=len(commands),errors=[dict(command=c['command'],exit_code=c['exit_code'],output=c.get('aggregated_output','')[:1000])for c in commands if c.get('exit_code')!=0],last_commands=[c['command']for c in commands[-2:]],latest_assistant=texts[-1][-7000:]if texts else None,terminal=(d/'01-source.result.json').exists())
 report[lane]=value
print(json.dumps(dict(at_utc=datetime.datetime.now(datetime.timezone.utc).isoformat(),lanes=report),indent=2))
