from pathlib import Path
import copy,difflib,hashlib,json,os,shutil,stat,subprocess
HERE=Path(__file__).resolve().parent
ROOT=HERE.parents[2]
BASE=ROOT/'ignored/proc-fd-design/cargo-v3/clippy'
old=json.loads((BASE/'plan.json').read_text());plan=copy.deepcopy(old)
def digest(p):
 h=hashlib.sha256()
 with Path(p).open('rb') as f:
  for b in iter(lambda:f.read(1024**2),b''):h.update(b)
 return h.hexdigest()
def bound(p):
 p=Path(p);s=p.stat();return {'path':str(p),'resolved_path':str(p.resolve(strict=True)),'bytes':s.st_size,'mode':s.st_mode&0o7777,'sha256':digest(p)}
rp=ROOT/'ignored/proc-fd-design/cargo-v3/execution-readback.json';r=json.loads(rp.read_text());artifact=r['compiled_executable'];assert artifact['sha256']=='b899a2eec14c966aa64dcadd2064177cd412e3a2e57abe68efbf2e327075a783'
listing=next(x for x in r['files'] if '/list/stdout' in x['path']);assert digest(listing['path'])==listing['sha256'];names=[x[:-6] for x in Path(listing['path']).read_text().splitlines() if x.endswith(': test')];assert len(names)==len(set(names))==427
assert digest(artifact['path'])==artifact['sha256']
for row in plan['inputs']:
 actual=bound(row['path']);assert actual==row,(row['path'],actual,row)
plan['status']='prepared only; one full normal native library attempt requires root release'
plan['observer']=old['stage']['argv'][2]
plan['observer_sha256']='f10ab861f262dbbd18295d92e59e05174299b72b397f58de844ee1725266eae6'
plan['source_commit']='696f0476aa46cf29e31b947a89379d80b4542ce3'
plan['landed_commit']='596b9adee8473dc0a7e62dce18580eead3d0c5c9'
plan['run_root']=str(HERE/'run-1');plan['tmpdir']=str(HERE/'run-1/tmp')
plan['observer_root']=str(Path(old['observer_root']).parents[1]/'native-full-v1')
plan['environment_fixed']['TMPDIR']=plan['tmpdir']
plan['registered_tests']=names;plan['registered_count']=427;plan['retained_listing']=listing;plan['retained_executable']=artifact
plan['clippy_prerequisite']={'summary':str(BASE/'run-1/summary.json'),'result':str(Path(old['stage']['out'])/'result.json')}
plan.pop('clippy_components',None)
payload=[artifact['path'],'--test-threads=1','--nocapture','-Z','unstable-options','--format=json']
stage={'name':'native-full','cpu_usec':30000000,'wall_seconds':60,'stderr_limit_bytes':1048576,'reader_limit_bytes':1048576,'cwd':str(ROOT),'out':str(Path(plan['observer_root'])/'native'),'payload':payload}
stage['argv']=['/usr/bin/python3','-B',plan['observer'],'--out',stage['out'],'--cpu-usec',str(stage['cpu_usec']),'--wall-seconds',str(stage['wall_seconds']),'--log-bytes',str(stage['stderr_limit_bytes']),*payload];plan['stage']=stage
plan['execution']=['/usr/bin/python3','-B',str(HERE/'launch.py')]
plan['preparation_changes']=[
 'Reuses exact successful source-v3 Rust test ELF and retained427 registration listing, without Cargo rebuild or a new listing invocation. Prior37 results and all earlier failures remain bound.',
 'Runs the full normal library population with no filters, --include-ignored or skip. Keeps one test thread and --nocapture; adds only nightly libtest JSON formatting so parent events cannot be confused with nested exact-test child transcripts.',
 'Same native30 aggregate CPU seconds/60 wall seconds,16GiB memory/zero swap,1MiB fatal wrapper stderr and bounded caller output reads. Unchanged observer separately caps each raw stdio file at64MiB. No completion guarantee or automatic retry.',
 'Actual full population includes52 ptraced initialization child cases, real KVM microprogram controls, isolated standard-descriptor/SIGPIPE subprocesses, and two C ABI probes that compile their existing C fixtures. These remain ordinary test behavior; no Rust build or target mutation is requested.',
 'Existing test-side conditional KVM-unavailable returns and helper no-op entries remain unchanged and must be reported separately from libtest ignored counts. No new requirement flag is added to the environment.',
 'Native VM and executor tests are component controls, not a Hermit canonical guest, cross-backend parity or determinism measurement.',
 'Fresh observer output subtree is proposed, not created by preparation; root must authorize it with the concrete execution release.'
]
plan['provenance_limit']='Exact retained Rust ELF/listing/source/lock, supervisor, tool entrypoints and actual test outcomes. The C ABI controls retain their normal compiler invocation; compiler/header transitive provenance is not exhaustively frozen. A successful full library summary is not evidence that source-defined KVM-unavailable early returns executed their VM bodies.'
extra=[rp,ROOT/'ignored/proc-fd-design/cargo-v3/run-1/summary.json',ROOT/'ignored/proc-fd-design/cargo-v3/run-1/launch.json',ROOT/'ignored/proc-fd-design/cargo-v3/run-1/compiled-executable.json',Path(artifact['path']),Path(listing['path']),BASE/'execution-readback.json',BASE/'run-1/summary.json',Path(old['stage']['out'])/'result.json']
native_plan=json.loads(Path(plan['native_prerequisite']['plan']).read_text())
for st in native_plan['stages']:
 for name in ['result.json','stdout','stderr']:extra.append(Path(st['out'])/name)
for tool in ['cc','timeout']:
 resolved=shutil.which(tool,path=plan['environment_fixed']['PATH']);assert resolved;extra.append(Path(resolved))
for path in extra:
 row=bound(path)
 if not any(x['path']==row['path'] for x in plan['inputs']):plan['inputs'].append(row)
plan_path=HERE/'plan.json';plan_path.write_text(json.dumps(plan,indent=2)+'\n')
source=(BASE/'launch.py').read_text();source=source[:source.index('\ndef main():')]
source=source.replace('Execute one explicitly released Clippy check after bound native controls pass.','Execute one released full native library run of the retained source-v3 ELF.')
source=source.replace("PLAN_SHA256 = 'a13af803508b0175c78755499b366801312da4cf8fbd584b7162c2b11bc89ec5'",'PLAN_SHA256 = '+repr(digest(plan_path)))
source += r'''

def check_population(plan, artifact):
    require(artifact == plan['retained_executable'], 'native executable differs from the reviewed retained artifact')
    check_executable(artifact)
    raw = read_bounded(plan['retained_listing']['path'], 1024**2)
    require(hashlib.sha256(raw).hexdigest() == plan['retained_listing']['sha256'], 'retained listing changed')
    names = [line[:-6] for line in raw.decode().splitlines() if line.endswith(': test')]
    require(len(names) == len(set(names)) == plan['registered_count'] == 427
            and names == plan['registered_tests'], 'retained complete population differs')
    expected = [artifact['path'], '--test-threads=1', '--nocapture',
                '-Z', 'unstable-options', '--format=json']
    step = plan['stage']
    require(step['payload'] == expected, 'full native payload changed or gained a selection')
    require(step['argv'] == ['/usr/bin/python3', '-B', plan['observer'], '--out', step['out'],
                            '--cpu-usec', '30000000', '--wall-seconds', '60',
                            '--log-bytes', '1048576', *expected], 'actual observer argv differs')
    require(step['cpu_usec'] == 30000000 and step['wall_seconds'] == 60
            and step['stderr_limit_bytes'] == step['reader_limit_bytes'] == 1048576,
            'native resource/read limits changed')
    return names


def parse_native_output(raw, stderr, plan):
    events = []
    parse_errors = []
    for index, line in enumerate(raw.decode(errors='replace').splitlines()):
        if not line.startswith('{'):
            continue
        try:
            event = json.loads(line)
        except (ValueError, TypeError) as error:
            parse_errors.append({'line': index + 1, 'error': str(error)})
            continue
        if isinstance(event, dict) and event.get('type') in ['suite', 'test']:
            events.append(event)
    outcomes = [e for e in events if e.get('type') == 'test'
                and e.get('event') in ['ok', 'failed', 'ignored']]
    starts = [e for e in events if e.get('type') == 'suite' and e.get('event') == 'started']
    summaries = [e for e in events if e.get('type') == 'suite' and e.get('event') in ['ok', 'failed']]
    counts = {status: sum(e.get('event') == status for e in outcomes) for status in ['ok', 'failed', 'ignored']}
    unavailable = [line for raw_stream in [raw, stderr]
                   for line in raw_stream.decode(errors='replace').splitlines()
                   if 'skipping' in line.lower() and 'kvm' in line.lower()]
    return {'events': events, 'outcomes': outcomes, 'outcome_counts': counts,
            'suite_starts': starts, 'suite_summaries': summaries, 'json_parse_errors': parse_errors,
            'kvm_unavailable_diagnostics': unavailable,
            'stdout_sha256': hashlib.sha256(raw).hexdigest(), 'stderr_sha256': hashlib.sha256(stderr).hexdigest(),
            'expected_registered_count': plan['registered_count'],
            'scope': 'Actual normal libtest outcomes; conditional early returns are not libtest ignored tests.'}


def require_complete_population(parsed, names):
    outcomes = parsed['outcomes']
    require(not parsed['json_parse_errors'], 'native output contained a malformed JSON event or diagnostic line')
    require(len(parsed['suite_starts']) == 1
            and parsed['suite_starts'][0].get('test_count') == len(names), 'full native suite start differs')
    observed_names = [e.get('name') for e in outcomes]
    require(len(outcomes) == len(names) and len(set(observed_names)) == len(names)
            and set(observed_names) == set(names), 'native outcome population missing or duplicated')
    require(len(parsed['suite_summaries']) == 1, 'native suite did not retain exactly one terminal summary')
    summary = parsed['suite_summaries'][0]
    counts = parsed['outcome_counts']
    require(summary.get('passed') == counts['ok'] and summary.get('failed') == counts['failed']
            and summary.get('ignored') == counts['ignored'] and summary.get('measured') == 0
            and summary.get('filtered_out') == 0, 'native terminal totals disagree with individual outcomes')
    require(summary.get('event') == 'ok' and counts['failed'] == 0, 'full native suite contains failures')


def main():
    require(digest(PLAN) == PLAN_SHA256, 'concrete full native plan changed')
    plan = json.loads(read_bounded(PLAN, 1024**2))
    root = Path(plan['run_root'])
    require(root == HERE / 'run-1', 'unexpected full native output destination')
    require(plan['stage']['name'] == 'native-full', 'unexpected native stage')
    for path in [root, Path(plan['observer_root'])]:
        require(not path.exists() and not path.is_symlink(), 'retain every earlier attempt: ' + str(path))
    check_inputs(plan)
    environment = {key: os.environ[key] for key in plan['environment_keys'] if key in os.environ}
    environment.update(plan['environment_fixed'])
    native = check_native_prerequisite(plan, environment)
    names = check_population(plan, native['artifact'])
    clippy = plan['clippy_prerequisite']
    clippy_summary = json.loads(read_bounded(clippy['summary'], 1024**2))
    require(clippy_summary['status'] == 'passed' and clippy_summary['exit'] == 0, 'prior Clippy success differs')
    clippy_result = json.loads(read_bounded(clippy['result'], 1024**2))
    root.mkdir(mode=0o700)
    Path(plan['tmpdir']).mkdir(mode=0o700)
    write_new(root / 'native-prerequisite-readback.json', native)
    write_new(root / 'launch.json', {'plan_sha256': PLAN_SHA256, 'caller_sha256': digest(__file__),
              'source_binding_sha256': digest(plan['source_binding']), 'environment': environment,
              'scope': 'One full native library run, including its existing native VM/C controls; no Hermit canonical parity claim.'})
    step = plan['stage']
    parsed = None
    try:
        require_terminal(clippy_result, 0, {'name': 'prior-clippy'}, root, environment)
        check_inputs(plan)
        check_population(plan, native['artifact'])
        write_new(root / 'native-full-dispatch.json', {'argv': step['argv'], 'cwd': step['cwd'],
                  'native_artifact': native['artifact'], 'registered_tests': names})
        with (root / 'native-full-observer.stdout').open('xb') as stdout:
            with (root / 'native-full-observer.stderr').open('xb') as stderr:
                process = subprocess.run(step['argv'], cwd=step['cwd'], env=environment,
                                         stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr)
        result_path = Path(step['out']) / 'result.json'
        require(result_path.is_file(), 'observer did not retain a native result')
        result = json.loads(read_bounded(result_path, 1024**2))
        write_new(root / 'native-full-readback.json', {'observer_exit': process.returncode,
                  'result_path': str(result_path), 'result_sha256': digest(result_path), 'result': result})
        raw = read_bounded(Path(step['out']) / 'stdout', step['reader_limit_bytes'])
        stderr = read_bounded(Path(step['out']) / 'stderr', step['reader_limit_bytes'])
        parsed = parse_native_output(raw, stderr, plan)
        write_new(root / 'native-outcomes.json', parsed)
        # Preserve all outcomes before checking success; a failure does not trigger a retry.
        require_terminal(result, process.returncode, step, root, environment)
        check_inputs(plan)
        check_executable(native['artifact'])
        require_complete_population(parsed, names)
    except Exception as error:
        write_new(root / 'summary.json', {'status': 'failed', 'error': str(error),
                  'outcome_counts': None if parsed is None else parsed['outcome_counts'],
                  'scope': 'Retained native outcome or preparation/accounting failure; no repair or retry.'})
        raise
    write_new(root / 'summary.json', {'status': 'passed', 'exit': result['wrapper_exit_code'],
              'aggregate_cpu_nsec': result['final_accounting']['cpu_usage_nsec'],
              'wall_seconds': result['elapsed_seconds'], 'registered_count': len(names),
              'outcome_counts': parsed['outcome_counts'], 'native_artifact': native['artifact'],
              'kvm_unavailable_diagnostics': parsed['kvm_unavailable_diagnostics'],
              'scope': 'Normal library outcomes only; no Hermit canonical guest or cross-backend qualification.'})
    print(json.dumps({'status': 'passed', 'summary': str(root / 'summary.json')}))


if __name__ == '__main__':
    main()
'''
(HERE/'launch.py').write_text(source)
(HERE/'caller.diff').write_text(''.join(difflib.unified_diff((BASE/'launch.py').read_text().splitlines(True),source.splitlines(True),fromfile='cargo-v3/clippy/launch.py',tofile='native-full-v1/launch.py')))
(HERE/'plan-differences.json').write_text(json.dumps({k:{'before':old.get(k),'after':plan.get(k)} for k in sorted(set(old)|set(plan)) if old.get(k)!=plan.get(k)},indent=2)+'\n')
print('ROOT',ROOT);print('caller',digest(HERE/'launch.py'));print('plan',digest(plan_path));print('delta',digest(HERE/'caller.diff'));print('inputs',len(plan['inputs']));print('proposed observer',plan['observer_root'])
