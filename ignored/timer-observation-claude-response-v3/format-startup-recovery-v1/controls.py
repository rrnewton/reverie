from pathlib import Path
import hashlib,json,os,sys
D=Path(__file__).resolve().parent;Q=D.parent/'positive-v2-continuation';sys.path.insert(0,str(Q))
import cache_lease as C
from common import file_record,write_new
P=D/'PROOF.json';old=json.loads(P.read_text())['plan']['sha256'];results=[]
for name,mutate,old_sha in [('bound',False,old),('changed-proof',True,old),('other-phase',False,'0'*64)]:
 root=D/('control-'+name);root.mkdir();state=root/'state';state.mkdir();lease=root/'lease';lease.write_bytes(C.token(old_sha));lease.chmod(0o600);st=lease.stat()
 b=dict(path=str(lease),owner_slot=str(root),identity=[st.st_dev,st.st_ino,st.st_uid],state_directory=str(state))
 if mutate:
  changed=json.loads(P.read_text());changed['accepted']=True;p=root/'proof.json';p.write_text(json.dumps(changed));record=file_record(p)
 else:record=file_record(P)
 write_new(state/(old_sha+'.json'),dict(plan_sha256=old_sha,result=record))
 error=None;fd=None
 try:fd=C.claim(b,'a'*64)
 except Exception as e:error=type(e).__name__+': '+str(e)
 finally:
  if fd is not None:os.close(fd)
 changed=lease.read_bytes()!=C.token(old_sha)
 assert (error is None and changed) if name=='bound' else (error is not None and not changed)
 results.append(dict(name=name,accepted=error is None,error=error,token_changed=changed))
write_new(D/'CONTROLS.json',dict(source=file_record(Q/'cache_lease.py'),proof=file_record(P),controls=results));print(results)
