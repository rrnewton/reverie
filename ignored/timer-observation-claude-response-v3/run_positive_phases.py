import hashlib,json,subprocess,sys
from pathlib import Path
q=Path(__file__).resolve().parent/sys.argv[1]
for name in ['list-lib','list-static','test-correction-lib','test-correction-vm','test-terminal-exec-lib','test-exec-vm','test-alarm-lib','test-child-lib','test-domain-lib','test-alarm-vm','test-child-vm','format','clippy']:
    prepare=subprocess.run([sys.executable,'-B',str(q/'prepare.py'),name],capture_output=True)
    (q/(name+'-prepare.stdout')).write_bytes(prepare.stdout);(q/(name+'-prepare.stderr')).write_bytes(prepare.stderr)
    if prepare.returncode: raise SystemExit('prepare failed '+name+': '+prepare.stderr.decode())
    plan=q/(name+'-plan.json');sha=hashlib.sha256(plan.read_bytes()).hexdigest()
    run=subprocess.run([sys.executable,'-B',str(q/'phase.py'),'launch',str(plan),sha],capture_output=True)
    (q/(name+'-launch.stdout')).write_bytes(run.stdout);(q/(name+'-launch.stderr')).write_bytes(run.stderr)
    result=json.loads((q/'controls'/name/'result.json').read_text())
    print(json.dumps({'phase':name,'caller_rc':run.returncode,'accepted':result['accepted'],'raw_status':result['raw_status'],'terminal_authenticated':result['terminal_authenticated'],'readback_error':result.get('readback_error'),'error':result.get('error')}),flush=True)
    if not result['accepted']: raise SystemExit('phase failed '+name)
