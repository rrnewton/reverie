from pathlib import Path
import hashlib,json,os,shutil,subprocess,sys
D=Path(__file__).resolve().parent;P=D/'positive-v2';Q=D/'positive-v2-continuation'
ret=P/'retained';ret.mkdir(exist_ok=True);records={}
for name,v in json.loads((P/'controls/compile/artifacts.json').read_text()).items():
 src=Path(v['file']['path']);p=ret/(name+'.elf');assert hashlib.sha256(src.read_bytes()).hexdigest()==v['file']['sha256']
 if not p.exists():shutil.copyfile(src,p);p.chmod(0o555)
 assert hashlib.sha256(p.read_bytes()).hexdigest()==v['file']['sha256'];records[name]=dict(original=v['file'],retained=dict(path=str(p),bytes=p.stat().st_size,sha256=v['file']['sha256']),compile_result=str(P/'controls/compile/result.json'))
f=P/'RETAINED_ELFS.json'
if not f.exists():f.write_text(json.dumps(records,indent=2)+'\n')
for name in ['format','clippy']:
 p=subprocess.run([sys.executable,'-B',str(Q/'prepare.py'),name],capture_output=True);(Q/(name+'-prepare.stdout')).write_bytes(p.stdout);(Q/(name+'-prepare.stderr')).write_bytes(p.stderr)
 if p.returncode:raise SystemExit(p.stderr.decode())
 plan=Q/(name+'-plan.json');sha=hashlib.sha256(plan.read_bytes()).hexdigest()
 p=subprocess.run([sys.executable,'-B',str(Q/'phase.py'),'launch',str(plan),sha],capture_output=True);(Q/(name+'-launch.stdout')).write_bytes(p.stdout);(Q/(name+'-launch.stderr')).write_bytes(p.stderr)
 result=json.loads((Q/'controls'/name/'result.json').read_text());print(json.dumps(dict(phase=name,caller_rc=p.returncode,accepted=result['accepted'],raw_status=result['raw_status'],error=result.get('error'),readback_error=result.get('readback_error'))),flush=True)
 if not result['accepted']:raise SystemExit('phase did not qualify '+name)
