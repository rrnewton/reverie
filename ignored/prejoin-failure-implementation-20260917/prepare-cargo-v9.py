from pathlib import Path
import ast,hashlib,json,importlib.util
root=Path.cwd();a=root/'ignored/prejoin-failure-implementation-20260917';old=a/'cargo-v7';new=a/'cargo-v9';new.mkdir()
def digest(path):return hashlib.sha256(Path(path).read_bytes()).hexdigest()
def bind(path):
 p=Path(path);s=p.stat();return dict(path=str(p),resolved_path=str(p.resolve(strict=True)),bytes=s.st_size,mode=s.st_mode&0o7777,sha256=digest(p))
p=json.loads((old/'plan.json').read_text())
for row in p['inputs']:
 oldpath=row['path'];newpath=oldpath.replace('source-v9-preparation','source-v12-preparation')
 current=bind(newpath)
 if newpath==oldpath:assert current==row,'unchanged input differs: '+oldpath
 row.clear();row.update(current)
p['source_binding']=str(a/'source-v12-preparation/binding.json');p['source_manifest']=str(a/'source-v12-preparation/tracked-source-manifest.json')
p['source_head']='d99853df1ab677863f149e6c81dfa2d1147f886d'
p['selected_tests']=json.loads((a/'source-v12-preparation/selected-tests.json').read_text())['selected']
p['required_count']=37;p['new_test_count']=17
p['status']='Prepared source-v12 correction: 37 exact no-VM controls, awaiting root execution release.'
for key in ['run_root','observer_root','target_dir','tmpdir']:
 p[key]=p[key].replace('cargo-v7','cargo-v9').replace('prejoin-native-v7','prejoin-native-v9')
for key in ['CARGO_TARGET_DIR','TMPDIR']:
 p['environment_fixed'][key]=p['environment_fixed'][key].replace('cargo-v7','cargo-v9').replace('prejoin-native-v7','prejoin-native-v9')
p['execution']=['/usr/bin/python3','-B',str(new/'launch.py')]
for stage in p['stages']:
 stage['out']=stage['out'].replace('cargo-v7','cargo-v9')
 stage['argv']=[s.replace('cargo-v7','cargo-v9') for s in stage['argv']]
 if stage['name']=='native':
  stage['payload']=['<verified-compiled-test-executable>','--exact']+p['selected_tests']+['--test-threads=1','--nocapture']
  stage['argv']=stage['argv'][:11]+stage['payload']
 assert stage['argv'][11:]==stage['payload']
p['preparation_changes']=[
 'Source-v12 corrects confirmed actual Claude findings; prior rejected commit and all evidence retained.',
 '37 exact no-VM selectors retain all 34 previous names, add two new native controls and one existing late-registration control.',
 'No manifest or lockfile changes. Source and full source manifest are byte-bound separately from the unchanged lock.',
 'Fresh caller/output/target directories; compile 600 CPU/900 wall, list 5 CPU/15 wall, native 30 CPU/60 wall, 16 GiB and zero swap, jobs 2 unchanged.',
 'Actual KVM held-hook fork/thread control, unchanged static_elf integration controls, and Hermit qualification remain separate; none execute here.'
]
report=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-parent-reader-support-20260916/ignored/prejoin-claude-review-d99853df/REPORT.md')
assert digest(report)=='3640d542d071297aa8247ee0aa7f6be864065d11acd7548df201862038830969'
p['inputs'].append(bind(report))
for path in [p['run_root'],p['observer_root'],p['target_dir']]:assert not Path(path).exists() and not Path(path).is_symlink()
(new/'plan.json').write_text(json.dumps(p,indent=2)+'\n')
script=(old/'launch.py').read_text().replace(digest(old/'plan.json'),digest(new/'plan.json'))
for oldtext,newtext in [('== 34','== 37'),("'count': 34","'count': 37"),("('34', '0', '0', '0')","('37', '0', '0', '0')"),('exactly 34 passing','exactly 37 passing'),("'selected_count': 34","'selected_count': 37")]:script=script.replace(oldtext,newtext)
needle="    root = Path(plan['run_root'])\n"
assert script.count(needle)==1
script=script.replace(needle,"    for step in plan['stages']:\n        require(step['argv'][11:] == step['payload'], 'stage payload disagrees with argv')\n    require(plan['stages'][2]['payload'] == ['<verified-compiled-test-executable>', '--exact'] + plan['selected_tests'] + ['--test-threads=1', '--nocapture'], 'native selectors disagree with payload')\n"+needle)
ast.parse(script);(new/'launch.py').write_text(script)
spec=importlib.util.spec_from_file_location('prejoin_cargo_v9',new/'launch.py');helper=importlib.util.module_from_spec(spec);spec.loader.exec_module(helper);helper.check_inputs(p)
record={'plan':bind(new/'plan.json'),'caller':bind(new/'launch.py'),'binding':bind(p['source_binding']),'observer':bind(p['observer']),'outputs':{s['name']:s['out'] for s in p['stages']},'execution':p['execution'],'preflight':'Full input/source checks passed; no execution.'}
(new/'reservation.json').write_text(json.dumps(record,indent=2)+'\n');print(json.dumps(record,indent=2))
