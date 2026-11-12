from pathlib import Path
import ast, collections, copy, difflib, hashlib, json, os, re, shutil

N = Path(__file__).resolve().parent
P = N.parent / 'rdtsc-recovery-source-v2'
V1 = N.parent / 'rdtsc-recovery-source-v1'
B = N.parent / 'rdtsc-fault-baseline-v1'
BASE = N.parent / 'publication-fd-composition-v3/source'
R = N.parents[1]
Q = N / 'qualification-v1'
CHANGED = 'reverie-kvm/tests/static_elf.rs'
NEW_TEST = 'fault_only_assertion_rejects_actual_tool_exit_cleanup_failure'

def sha(b): return hashlib.sha256(b).hexdigest()
def rec(p):
    p = Path(p); st = p.lstat()
    if p.is_symlink():
        b = os.readlink(p).encode()
        return dict(path=str(p), kind='symlink', target=os.readlink(p), bytes=len(b), sha256=sha(b), mode=st.st_mode & 0o7777)
    b = p.read_bytes()
    return dict(path=str(p), kind='file', bytes=len(b), sha256=sha(b), mode=st.st_mode & 0o7777)
def write(path, value):
    p = N / path; p.parent.mkdir(parents=True, exist_ok=True)
    with p.open('x') as f: f.write(value if isinstance(value, str) else json.dumps(value, indent=2) + '\n')
def diff(before, after, path, old='v2', new='v3'):
    return ''.join(difflib.unified_diff(before.splitlines(keepends=True), after.splitlines(keepends=True), fromfile=old+'/'+path, tofile=new+'/'+path))

source = []; changed = []
for row in json.loads((P/'SOURCE-MANIFEST.json').read_text()):
    new = copy.deepcopy(row); new['path'] = str(N/'source'/row['relative'])
    path = Path(new['path'])
    if row['kind'] == 'unexpanded_gitlink':
        assert path.is_dir() and not list(path.iterdir())
    else:
        old_actual = rec(P/'source'/row['relative']); actual = rec(path)
        assert old_actual['sha256'] == row['sha256']
        assert actual['mode'] == old_actual['mode']
        if actual['sha256'] != row['sha256']:
            assert row['relative'] == CHANGED
            changed.append(dict(relative=CHANGED, before=old_actual, after=actual))
            new['sha256'] = actual['sha256']; new['bytes'] = actual['bytes']
    source.append(new)
assert len(changed) == 1 and len(source) == 2622
write('SOURCE-MANIFEST.json', source)
before = (P/'source'/CHANGED).read_text(); after = (N/'source'/CHANGED).read_text()
write('DELTA.patch', diff(before, after, CHANGED))
paths = [r['relative'] for r in json.loads((P/'qualification-v1/SETUP.json').read_text())['source_files']]
full = ''
for path in paths:
    original = (BASE/path).read_text() if (BASE/path).exists() else ''
    full += diff(original, (N/'source'/path).read_text(), path, 'a', 'b')
write('SOURCE.patch', full)

# Authenticate all existing test bodies, allowing only the explicitly authorized
# strengthening of the candidate single-step error-shape check.
body_records = []
for name in re.findall(r'^fn (\w+)\(', before, re.M):
    start = before.index('fn '+name+'(')
    if not before[max(0,start-40):start].rstrip().endswith('#[test]'): continue
    match = re.search(r'^}\n', before[start:], re.M)
    assert match, name
    body = before[start:start+match.end()]
    replacement = body
    if name == 'timestamp_single_step_reports_the_retired_instruction_boundary':
        replacement = body.replace('let error = completion.result.unwrap_err();', 'let error = unwrap_shared_guest_exception(completion.result.unwrap_err());').replace('matches!(error.primary(),', 'matches!(&error,')
        assert replacement != body
    assert after.count(replacement) == 1, name
    body_records.append(dict(name=name, bytes=len(body.encode()), sha256=sha(body.encode()), unchanged=replacement==body, after_sha256=sha(replacement.encode())))
assert NEW_TEST not in [r['name'] for r in body_records]
assert after.count('fn '+NEW_TEST+'(') == 1
write('TEST-BODY-CONTINUITY.json', dict(predecessor_tests=body_records, preserved_count=sum(r['unchanged'] for r in body_records), new_test=NEW_TEST,
    numeric_assertions_preserved=True, production_files_changed=[], helper_change='Exactly one SharedFailure with immediate GuestException only; no recursive primary() flattening.'))
write('SOURCE-CONTINUITY.json', dict(base='79516661bf82d30ab2967c71834a6d47447b76ee', landed_equivalent_base='44fcb1955f44547f50d724fe8f7d718215fed446', base_tree='7620fe83f486d665d9d09d4f09f0e93636b862e4', predecessor_target=rec(P/'TARGET.json'), entries=len(source), exact_changed_entries=changed, all_other_bytes_modes_links_unchanged=True, production_unchanged=True, test_body_continuity=rec(N/'TEST-BODY-CONTINUITY.json')))

Q.mkdir()
for name in ['prepare.py','phase.py','common.py','cache_lease.py','bind_dependencies.py','admit_target.py','retain_artifacts.py','SELECTORS.json','toolchain-standard-inputs.json','PREDECESSOR-ELFS.json']:
    shutil.copyfile(P/'qualification-v1'/name, Q/name)
(Q/'observer').mkdir()
for p in (P/'qualification-v1/observer').iterdir():
    if p.is_file() and (p.suffix == '.py' or p.name == 'source-inputs.json'):
        shutil.copyfile(p, Q/'observer'/p.name)
caller_delta = ''
for name, count in [('prepare.py',2),('admit_target.py',1)]:
    p = Q/name; old = p.read_text()
    assert old.count('rdtsc-recovery-v2-cold-v1') == count
    new = old.replace('rdtsc-recovery-v2-cold-v1','rdtsc-recovery-v3-cold-v1')
    if name == 'prepare.py':
        assert new.count('exact 30 declarations') == 1
        new = new.replace('exact 30 declarations','exact 31 declarations')
    p.write_text(new); caller_delta += diff(old,new,'qualification-v1/'+name)
old_selection = (P/'qualification-v1/SELECTORS.json').read_text()
selection = json.loads(old_selection)
selection['groups']['timestamp-31'] = dict(artifact='static', names=[NEW_TEST], source=CHANGED, origin='V3 new actual cleanup rejection control', purpose='Actual UD2 Tool run plus failing on_exit_thread must retain both typed causes; existing fault-only helper must reject the complete cleanup aggregate. No synthetic error object or production variant.')
selection['exact_declarations'] = 31
(Q/'SELECTORS.json').write_text(json.dumps(selection,indent=2)+'\n')
caller_delta += diff(old_selection,(Q/'SELECTORS.json').read_text(),'qualification-v1/SELECTORS.json')
order = json.loads((P/'RUN-ORDER.json').read_text()); index = order['phases'].index('timestamp-19') + 1
order['phases'].insert(index, 'timestamp-31')
assert len(order['phases']) == 40 and len(set(order['phases'])) == 40
write('RUN-ORDER.json',order)
caller_delta += diff((P/'RUN-ORDER.json').read_text(),(N/'RUN-ORDER.json').read_text(),'RUN-ORDER.json')
write('CALLER-DELTA.patch', caller_delta)
shutil.copyfile(P/'execute_phases.py', N/'execute_phases.py')
setup = json.loads((P/'qualification-v1/SETUP.json').read_text())
setup['source_root'] = str(N/'source'); setup['target'] = str(R/'target/rdtsc-recovery-v3-cold-v1')
for row in setup['source_files']:
    actual = rec(N/'source'/row['relative']); row['file'] = {k:actual[k] for k in ['path','bytes','mode','sha256']}
actual = rec(N/'source/Cargo.lock'); setup['lock'] = {k:actual[k] for k in ['path','bytes','mode','sha256']}
write('qualification-v1/SETUP.json',setup)
phase_manifest = json.loads((P/'qualification-v1/source-manifest.json').read_text())
for row in phase_manifest:
    if row['path'] == CHANGED: row['sha256'] = sha(after.encode())
write('qualification-v1/source-manifest.json',phase_manifest)
origins = []
for p in sorted(Q.rglob('*.py')):
    ast.parse(p.read_text()); old = P/'qualification-v1'/p.relative_to(Q)
    origins.append(dict(before=rec(old),after=rec(p),byte_identical=old.read_bytes()==p.read_bytes()))
write('qualification-v1/RUNNER_ORIGINS.json', dict(sources=origins, material_delta=rec(N/'CALLER-DELTA.patch'), policy='Only new target spelling and descriptive scope count change in helpers. One additional exact selector and order entry. Observer, phase/common/lease, dependency/artifact logic and all limits unchanged.', executed=False))
controls = []
for phase, group in selection['groups'].items():
    text = (N/'source'/group['source']).read_text(); name = group['names'][0]; leaf = name.rsplit('::',1)[-1]
    found = list(re.finditer(r'\bfn '+re.escape(leaf)+r'\(',text)); assert len(found) == 1,name
    controls.append(dict(phase=phase, target=group['artifact'], selector=name, source=group['source'], line=text[:found[0].start()].count('\n')+1, source_sha256=sha(text.encode()), origin=group['origin'], purpose=group['purpose'], executed=False, outer_cpu_seconds=30, outer_wall_seconds=60))
write('CONTROLS.json',dict(status='Planned only; actual V3 inventory and outcomes absent', declarations=len(controls), targets=dict(collections.Counter(r['target'] for r in controls)), controls=controls))

write('REPORT.md', '''# V3: preserve fault assertions through the documented shared-error API

This is author work, not an independent source approval. V3 is uncompiled and unrun. All production source is identical to V2. Only static_elf.rs changes: a private helper unwraps bare GuestException or exactly one SharedFailure whose immediate payload is GuestException, and one real cleanup-rejection control is added. Every historical test body, fault vector, RIP and page-fault-address assertion remains byte-identical. The candidate single-step control is additionally strengthened: its previous primary() match now uses the same narrow envelope helper, retaining exact vector 1, next RIP and callback count while refusing cleanup/effect context. UD/GP still do not invent a CR2=0 requirement. No primary() flattening, warning allowance, production ownership change, extra skip or comparator relaxation is introduced.

V2 timestamp-10 really failed raw 101 / accepted=false at its first UD2 Tool branch. Its displayed error was vector 6 at RIP 0x200000; the later GP branch was not reached. The same unchanged predecessor neighbor on a retained source-qualified 585 ELF then failed raw 101 / accepted=false at the same helper. No KVM skip was emitted. Those results prove the helper incompatibility predates this timestamp candidate; the SharedFailure variant is a source-derived explanation, since those old outputs logged Display rather than Debug. Their original receipts are retained verbatim. V1's raw-0 unused_mut diagnostic refusal also remains a refusal.

The current production completion API deliberately publishes an Arc and returns SharedFailure. A helper that accepts Error::primary() indiscriminately would hide WithCleanup, SignalEffects or worker context. This correction accepts only the one ownership envelope and leaves all other shapes rejected. The new test creates a real UD2 ELF and a Tool whose on_exit_thread returns EIO. Before testing rejection it requires the actual returned WithCleanup to retain a single shared GuestException vector 6/RIP 0x200000 and a single shared Reverie EIO cleanup cause. Only the fault assertion is inside catch_unwind; guest execution and the typed aggregation assertions are outside it. The test requires that fault-only assertion to reject the aggregate. The expected caught assertion panic remains visible in raw stderr; no test-suite failed/ignored outcome is allowed. Nothing is fabricated by constructing an error value.

The positive predecessor direct/Tool fault control is already timestamp-19. Its unchanged body covers direct UD, Tool UD, direct PF and direct GP if it completes. V3 adds only timestamp-31, yielding 31 declarations and 40 phases. The new negative runs immediately after timestamp-19 so both helper contracts are checked before later neighbors. Existing selectors and their relative ordering remain intact. New V3 ELF and exact inventory are required; V2's nine passing declarations are historical evidence, not V3 qualification. There is no credit yet for V3 PF, GP, cleanup rejection, native architecture probes or same-run 75-cell parity.

Source path counts and prior test bodies are mechanically bound in SOURCE-CONTINUITY and TEST-BODY-CONTINUITY. Caller changes are only the fresh target spelling, descriptive declaration count and one additive selector/order entry. Every observer/accounting/lease/source/loader/dependency gate and resource limit is unchanged. Existing old controls that can print a KVM-skip message require a separate full raw-output no-skip audit; a libtest ok alone is insufficient. Source reviewers remain held until actual qualification is attached and root authorizes their launch.
''')
write('PLAN.md',f'''# V3 finite qualification plan — frozen preparation, awaiting root inspection

Target source: {N/'source'}. Planned new empty cache: {setup['target']}. This preparation creates no target, claims no lease and executes no compiler, test or reviewer.

Follow the established V1 CALLER-PLAN and exact RUN-ORDER with 31 declarations / 40 phases. Use explicit PYTHONOPTIMIZE=0 /usr/bin/python3 -B and record optimize=0/debug=true for the assert-using outer driver. Root must inspect this source/caller delta before execution. Admit the new empty target, run metadata, freshly bind its actual dependency closure, compile, retain new source-bound distinct-inode ELF copies, then continue exact remaining phases. Stop at the first unaccepted phase. No retry, waiver, reused emitted ELF or expected failing suite is allowed.

All bounds remain: metadata/compile/check 600 aggregate CPU seconds / 900 wall; format/list/each test 30 CPU / 60 wall; two jobs, offline/locked, unchanged dated toolchain and strict zero diagnostics; 16 GiB memory/no swap, stderr 16 MiB/stdout 64 MiB/phase read 16 MiB, free floor 100 GiB. Existing internal guest/clock wrappers remain unchanged. Original observer137c and phase/common/lease/source/dependency/loader checks are byte-identical. Live R source, HEAD, index and branch remain unchanged.

Fresh Cargo JSON must identify all four test harnesses and the linked non-test KVM library as newly built (fresh=false). Only retained ELF copies run. Actual full inventories, actual exact selected outcomes, raw skip/diagnostic inspection and terminal accounting remain separate requirements. Check every test stdout/stderr for a skip or unexecuted capability; REVERIE_REQUIRE_KVM cannot retroactively strengthen old test bodies that do not consult it. The new cleanup control must execute the real Tool run, require its typed two-cause aggregate, and then reject that aggregate only through the helper under catch_unwind. Retain raw Debug and caught-panic output; the suite itself must pass without failure/ignore.

Keep V1 warning refusal, V2 timestamp-10 failure and source-qualified predecessor baseline failure as immutable first attempts. Later PF/GP or timestamp claims require actual completed selectors. Native hardware probes and a new H39ac/candidate same-run 75-cell ptrace/KVM comparison use separate authorized callers, unchanged complete comparator and original guest bounds; component success alone establishes neither.
''')
old_evidence = [V1/'qualification-refusal-v1/READBACK.json', P/'qualification-stop-v1/READBACK.json', B/'result-v1/READBACK.json']
write('TARGET.json', dict(status='IMMUTABLE V3 SOURCE/CALLER PREPARATION; NO EXECUTION', source=str(N/'source'), authorship_base='79516661bf82d30ab2967c71834a6d47447b76ee', landed_tree_equivalent_base='44fcb1955f44547f50d724fe8f7d718215fed446', base_tree='7620fe83f486d665d9d09d4f09f0e93636b862e4', predecessor=rec(P/'TARGET.json'), source_manifest=rec(N/'SOURCE-MANIFEST.json'), source_patch=rec(N/'SOURCE.patch'), v2_to_v3_delta=rec(N/'DELTA.patch'), caller_delta=rec(N/'CALLER-DELTA.patch'), setup=rec(Q/'SETUP.json'), source_continuity=rec(N/'SOURCE-CONTINUITY.json'), prior_failed_attempts=[rec(p) for p in old_evidence], selectors=rec(Q/'SELECTORS.json'), observer=rec(Q/'observer/observer.py'), new_target=setup['target'], production_unchanged=True, original_limits_and_selectors_retained=True, additive_declarations=1, total_declarations=31, total_phases=40))

records = {}
for p in sorted(N.rglob('*')):
    if p.is_symlink() or p.is_file(): records[str(p)] = rec(p)
for p in [V1/'TARGET.json',V1/'INPUTS.json',V1/'READBACK.json',V1/'PLAN.md',V1/'CALLER-PLAN.md',V1/'CONTROLS.json',P/'TARGET.json',P/'READBACK.json',P/'INPUTS.json',P/'DELTA.patch',B/'BINDING.json',B/'READBACK.json']:
    records[str(p)] = rec(p)
for directory in [V1/'qualification-refusal-v1',P/'qualification-stop-v1',B/'result-v1']:
    for p in sorted(directory.rglob('*')):
        if p.is_file(): records[str(p)] = rec(p)
# Expand existing immutable evidence indexes; retain actual raw results and
# binaries they bind, without importing or rewriting an earlier artifact.
for index in [V1/'qualification-refusal-v1/INPUTS.json', P/'qualification-stop-v1/INPUTS.json']:
    if index.exists():
        value = json.loads(index.read_text())
        for row in value.get('records',[]):
            p = Path(row['path']); actual = rec(p)
            assert actual['sha256'] == row['sha256']
            records[str(p)] = actual
for row in json.loads((B/'result-v1/REPORT.json').read_text()).values():
    if isinstance(row,dict) and 'path' in row and 'sha256' in row:
        actual = rec(row['path']); assert actual['sha256'] == row['sha256']; records[row['path']] = actual
write('INPUTS.json',dict(records=list(records.values()), source_entries=len(source), preparation_only=True))
for row in records.values(): assert rec(row['path']) == row
write('READBACK.json',dict(status='Frozen source/caller preparation only; root inspection required before execution', records=len(records), target=rec(N/'TARGET.json'), inputs=rec(N/'INPUTS.json'), delta=rec(N/'DELTA.patch'), caller_delta=rec(N/'CALLER-DELTA.patch'), source_entries=len(source), old_test_bodies_unchanged=sum(r['unchanged'] for r in body_records), strengthened_candidate_test='timestamp_single_step_reports_the_retired_instruction_boundary', production_unchanged=True, unchanged_helpers=sum(r['byte_identical'] for r in origins), modified_helpers=[r['after']['path'] for r in origins if not r['byte_identical']], no_execution=True))
print(json.dumps({name:rec(N/name) for name in ['TARGET.json','SOURCE.patch','DELTA.patch','CALLER-DELTA.patch','REPORT.md','PLAN.md','INPUTS.json','READBACK.json']},indent=2))
