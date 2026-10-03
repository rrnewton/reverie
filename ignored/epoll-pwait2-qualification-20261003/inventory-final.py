from pathlib import Path
import subprocess,json,hashlib
root=Path.cwd()
e=root/'ignored/epoll-pwait2-qualification-20261003/final-inventories'
e.mkdir(exist_ok=False)
entries={}
for family,name in [('library','reverie_kvm-7ad745a0abdfb62a'),('integration','static_elf-9a322fcc3d61e965')]:
 binary=root/'target/epoll-pwait2-qualification/debug/deps'/name
 for ignored in [False,True]:
  label=family+('-ignored' if ignored else '')
  cmd=[str(binary),'--list','--format','terse']+(['--ignored'] if ignored else [])
  r=subprocess.run(cmd,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=15)
  (e/(label+'-inventory.stdout')).write_bytes(r.stdout)
  (e/(label+'-inventory.stderr')).write_bytes(r.stderr)
  assert r.returncode==0,(label,r.returncode,r.stderr)
  names=[s.removesuffix(': test') for s in r.stdout.decode().splitlines() if s.endswith(': test')]
  assert len(names)==len(set(names))
  entries[label]={'names':names,'count':len(names),'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest()}
assert entries['library']['count']==1111
assert entries['integration']['count']==427
(e/'BINARY-INVENTORY.json').write_text(json.dumps(entries,indent=2)+'\n')
print(json.dumps({k:{'count':v['count'],'binary_sha256':v['binary_sha256']} for k,v in entries.items()}))

declared=json.loads((e.parent/'DECLARED-POPULATION-FINAL.json').read_text())
assert set(declared['new_unit']) <= set(entries['library']['names'])
assert set(declared['integration']) <= set(entries['integration']['names'])
assert entries['library-ignored']['count']==entries['integration-ignored']['count']==0
