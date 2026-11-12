"""Outer orchestration only: invoke unchanged prepared phase and preserve raw return status."""
import hashlib,json,subprocess,sys,time
from pathlib import Path
N=Path(__file__).resolve().parent
Q=N/'qualification-preparation-v1'
planned=json.loads((N/'RUN-ORDER.json').read_text())['phases']
for name in sys.argv[1:]:
    assert name in planned and not(Q/(name+'-plan.json')).exists(), name
    for kind,args in [('prepare',['/usr/bin/python3','-B',str(Q/'prepare.py'),name]),('launch',None)]:
        if args is None:
            p=Q/(name+'-plan.json')
            args=['/usr/bin/python3','-B',str(Q/'phase.py'),'launch',str(p),hashlib.sha256(p.read_bytes()).hexdigest()]
        start=time.monotonic()
        with (N/'launch'/(name+'-'+kind+'.stdout')).open('xb')as out,(N/'launch'/(name+'-'+kind+'.stderr')).open('xb')as err:
            result=subprocess.run(args,stdout=out,stderr=err)
        status=dict(argv=args,raw_status=result.returncode,wall_seconds=time.monotonic()-start)
        with (N/'launch'/(name+'-'+kind+'.status.json')).open('x')as f:
            json.dump(status,f,indent=2);f.write('\n')
        print(json.dumps(dict(phase=name,step=kind,raw_status=result.returncode,wall_seconds=status['wall_seconds'])),flush=True)
        if result.returncode:
            raise SystemExit(result.returncode)
