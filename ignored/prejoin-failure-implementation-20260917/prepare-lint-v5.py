from pathlib import Path
import json,hashlib,ast,runpy
r=Path.cwd();a=r/'ignored/prejoin-failure-implementation-20260917';old=a/'lint-v3';out=a/'lint-v5';out.mkdir()
def digest(p):return hashlib.sha256(Path(p).read_bytes()).hexdigest()
def bind(p):
 p=Path(p);s=p.stat();return {'path':str(p),'resolved_path':str(p.resolve(strict=True)),'bytes':s.st_size,'mode':s.st_mode&0o7777,'sha256':digest(p)}
p=json.loads((old/'plan.json').read_text())
for row in p['inputs']:
 oldpath=row['path'];newpath=oldpath.replace('source-v10-preparation','source-v13-preparation');current=bind(newpath)
 if oldpath==newpath:assert current==row,oldpath
 row.clear();row.update(current)
for k in ['source_binding','source_manifest']:p[k]=p[k].replace('source-v10-preparation','source-v13-preparation')
for k in ['run_root','observer_root','tmpdir']:p[k]=p[k].replace('lint-v3','lint-v5')
p['environment_fixed']['TMPDIR']=p['environment_fixed']['TMPDIR'].replace('lint-v3','lint-v5')
p['execution']=['/usr/bin/python3','-B',str(out/'launch.py')]
p['scope']='Read-only workspace formatting and Clippy on source-v13 after actual Claude corrections. No native, VM or guest execution.'
for s in p['stages']:
 s['out']=s['out'].replace('lint-v3','lint-v5');s['argv']=[v.replace('lint-v3','lint-v5') for v in s['argv']]
 assert s['payload']==s['argv'][s['argv'].index('--log-bytes')+2:]
for k in ['run_root','observer_root']:assert not Path(p[k]).exists()
(out/'plan.json').write_text(json.dumps(p,indent=2)+'\n')
script=(old/'launch.py').read_text().replace(digest(old/'plan.json'),digest(out/'plan.json'));ast.parse(script);(out/'launch.py').write_text(script)
f=runpy.run_path(p['helpers']['path'],run_name='preflight');f['check_inputs'](p)
record={'plan':bind(out/'plan.json'),'caller':bind(out/'launch.py'),'source':bind(p['source_binding']),'outputs':{s['name']:s['out'] for s in p['stages']},'preflight':'All inputs and full live source match. No execution.'};(out/'reservation.json').write_text(json.dumps(record,indent=2)+'\n');print(json.dumps(record,indent=2))
