import datetime, hashlib, json, os, pathlib, resource, signal, subprocess, sys, time
OUT = pathlib.Path(__file__).resolve().parent

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
        entry['resumes_target_session'] = '01a0b38e-9e3c-7883-9816-65199c048935' in args.split() and ('resume' in args.split() or '--resume' in args.split())
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

def write(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n')

stage = sys.argv[1]
if stage == 'census':
    c = census()
    write(OUT / 'capacity-latest.json', c)
    print(json.dumps(c, indent=2), flush=True)
    raise SystemExit(0)

prompt = OUT / (stage + '.prompt.txt')
assert prompt.is_file()
session = sys.argv[2] if len(sys.argv) == 3 else None
before = census()
write(OUT / (stage + '.capacity-before.json'), before)
if before['top_level_count'] >= before['cap']:
    write(OUT / (stage + '.REFUSED.json'), {'reason': 'top-level agent cap has no capacity', 'capacity': before})
    print('REFUSED: cap has no capacity', flush=True)
    raise SystemExit(75)
if any(row.get('resumes_target_session') for row in before['top_level_agents'] + before['native_children_collapsed']):
    write(OUT / (stage + '.REFUSED.json'), {'reason': 'target session already has a live resume', 'capacity': before})
    raise SystemExit(75)

argv = ['/usr/local/bin/codex', '-a', 'never', '-s', 'read-only', '-C', str(OUT), 'exec']
if session:
    argv += ['resume']
argv += ['-c', 'approval_policy=\"never\"', '--json', '--output-last-message', str(OUT / (stage + '.final.txt'))]
if session:
    argv += [session]
argv += ['-']
metadata = {'started_at_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(), 'argv': argv, 'model_override': None, 'prompt_sha256': hashlib.sha256(prompt.read_bytes()).hexdigest(), 'wall_limit_seconds': 1800, 'output_limit_bytes': 64 * 1024 * 1024, 'cwd': str(OUT), 'session_resume': session}
start = time.monotonic()
with prompt.open('rb') as inp, (OUT / (stage + '.jsonl')).open('wb') as stdout, (OUT / (stage + '.stderr')).open('wb') as stderr:
    proc = subprocess.Popen(argv, cwd=OUT, stdin=inp, stdout=stdout, stderr=stderr, start_new_session=True)
    metadata['launcher_pid'] = proc.pid
    metadata['launcher_pgid'] = os.getpgid(proc.pid)
    metadata['launcher_start_ticks'] = pathlib.Path('/proc', str(proc.pid), 'stat').read_text().rsplit(')', 1)[1].split()[19]
    write(OUT / (stage + '.launch.json'), metadata)
    print(json.dumps({'started': stage, 'pid': proc.pid, 'capacity_before': before['top_level_count']}), flush=True)
    reason = None
    while proc.poll() is None:
        elapsed = time.monotonic() - start
        total = sum((OUT / (stage + suffix)).stat().st_size for suffix in ('.jsonl', '.stderr'))
        if elapsed > 1800 or total > 64 * 1024 * 1024:
            reason = 'wall timeout' if elapsed > 1800 else 'raw output bound exceeded'
            os.killpg(proc.pid, signal.SIGTERM)
            try:
                proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                os.killpg(proc.pid, signal.SIGKILL)
            break
        time.sleep(0.5)
    rc = proc.wait()
metadata.update({'finished_at_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(), 'elapsed_seconds': time.monotonic() - start, 'raw_exit_code': rc, 'bound_failure': reason})
for suffix in ('.jsonl', '.stderr', '.final.txt'):
    path = OUT / (stage + suffix)
    if path.exists():
        data = path.read_bytes()
        metadata[ suffix.lstrip('.') ] = {'path': str(path), 'bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest()}
write(OUT / (stage + '.result.json'), metadata)
write(OUT / (stage + '.capacity-after.json'), census())
print(json.dumps({'finished': stage, 'raw_exit_code': rc, 'elapsed_seconds': metadata['elapsed_seconds'], 'bound_failure': reason}), flush=True)
raise SystemExit(rc if rc >= 0 else 128 - rc)
