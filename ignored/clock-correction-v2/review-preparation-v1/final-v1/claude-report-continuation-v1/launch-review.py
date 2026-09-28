import datetime,hashlib,json,pathlib,resource,subprocess,time
P=pathlib.Path(__file__).resolve().parent

def digest(p):
 h=hashlib.sha256()
 with pathlib.Path(p).open('rb') as f:
  for b in iter(lambda:f.read(1024*1024),b''):h.update(b)
 return h.hexdigest()
def write(n,d):
 with (P/n).open('x') as f:json.dump(d,f,indent=2);f.write('\n')
def census():
    raw = subprocess.run(['ps', '-eo', 'pid=,ppid=,uid=,stat=,comm=,args='], capture_output=True, text=True, check=True).stdout
    agents = []
    excluded = []
    for line in raw.splitlines():
        columns = line.split(None, 5)
        if len(columns) != 6:
            continue
        pid, ppid, uid, state, comm, args = columns
        if comm not in ('codex', 'claude'):
            continue
        entry = {'pid': int(pid), 'ppid': int(ppid), 'uid': int(uid), 'state': state, 'comm': comm, 'executable': args.split()[0]}
        entry['resumes_target_session'] = '--resume' in args.split() and target['session_id'] in args.split()
        if state.startswith('Z') or '--help' in args.split() or '-h' in args.split():
            entry['excluded_reason'] = 'zombie or help-only CLI probe; no live agent session'
            excluded.append(entry)
            continue
        try:
            entry['start_ticks'] = pathlib.Path('/proc', pid, 'stat').read_text().rsplit(')', 1)[1].split()[19]
        except FileNotFoundError:
            entry['excluded_reason'] = 'process exited during census'
            excluded.append(entry)
            continue
        agents.append(entry)
    pids = {a['pid']: a for a in agents}
    top = []
    wrappers = []
    for entry in agents:
        parent = pids.get(entry['ppid'])
        if parent and parent['comm'] == entry['comm']:
            wrappers.append({**entry, 'collapsed_into_launcher_pid': parent['pid']})
        else:
            top.append(entry)
    return {'at_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(), 'cap': 15, 'method': 'All-UID process census: exact comm codex/claude, exclude zombies/help-only probes, collapse direct same-CLI launcher/native child pairs only. Independently launched children via a shell/Python count separately even if an older agent is an ancestor. Code-mode, MCP, gclaude system service are not top-level codex/claude agents.', 'top_level_agents': top, 'top_level_count': len(top), 'native_children_collapsed': wrappers, 'excluded': excluded}

target=json.loads((P/'TARGET.json').read_text())
ground=pathlib.Path(target['grounding'])
complete=json.loads((ground/'COMPLETE.json').read_text());assert complete['complete'] and not complete['proposal_exposed'] and complete['session_id']==target['session_id']
for stage in ['01-paper','02-scheduler','03-vision','04-roadmap']:
 c=json.loads((ground/stage/'COVERAGE.json').read_text());assert c['qualified']
 assert json.loads((ground/stage/'exit.json').read_text())['raw_status']==0
packet=P/'inputs.json';bound=json.loads(packet.read_text())
def snapshot():
 for row in bound.get('symlinks',[]):
  assert pathlib.Path(row['path']).is_symlink() and str(pathlib.Path(row['path']).readlink())==row['target'], row['path']
 rows=[]
 for row in bound['files']:
  p=pathlib.Path(row['path']); actual={'path':str(p),'bytes':p.stat().st_size,'sha256':digest(p)}
  if 'mode' in row: actual['mode']=p.stat().st_mode & 0o7777
  assert actual==row,str(p)
  rows.append(actual)
 return rows
before=snapshot();write('source-before.json',before)
cmd=['timeout','--signal=TERM','--kill-after=10s','600s','with-proxy','claude','-p','--resume',target['session_id'],'--output-format','stream-json','--verbose','--permission-mode','plan','--tools','Read,Grep,Glob','--allowedTools','Read,Grep,Glob']
rec={'command':cmd,'cwd':str(ground),'session_id':target['session_id'],'scope':target['scope'],'head':target['head'],'base':target['base'],'tree':target['tree'],'frozen_source':target['frozen_source'],'started_at':datetime.datetime.now(datetime.timezone.utc).isoformat(),'wall_seconds':600,'kill_after_seconds':10,'per_output_file_bytes':67108864,'prompt_sha256':digest(P/'prompt.txt'),'packet_sha256':digest(packet),'input_count':len(before)}
write('launch.json',rec)
def limits():resource.setrlimit(resource.RLIMIT_FSIZE,(67108864,67108864))
capacity=census();write('capacity-before.json',capacity)
assert capacity['top_level_count'] < capacity['cap'], 'External CLI capacity unavailable'
assert not any(row.get('resumes_target_session') for row in capacity['top_level_agents'] + capacity['native_children_collapsed']), 'Target Claude session already has a live resume; root must await completion'
started=time.monotonic()
with (P/'prompt.txt').open('rb') as prompt,(P/'output.jsonl').open('xb') as output,(P/'stderr.log').open('xb') as stderr:
 r=subprocess.run(cmd,cwd=ground,stdin=prompt,stdout=output,stderr=stderr,preexec_fn=limits)
rec.update(exit_code=r.returncode,elapsed_seconds=time.monotonic()-started,completed_at=datetime.datetime.now(datetime.timezone.utc).isoformat())
after=snapshot();write('source-after.json',after);rec['inputs_unchanged']=before==after
write('exit.json',rec);print(json.dumps(rec))
assert rec['inputs_unchanged']
raise SystemExit(r.returncode)
