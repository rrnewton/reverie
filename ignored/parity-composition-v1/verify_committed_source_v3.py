from pathlib import Path
import hashlib,json,os,subprocess,sys
p=Path(__file__).resolve().parent
root=p.parents[1]
def require(ok,message):
 if not ok:raise RuntimeError(message)
def git(*a):return subprocess.check_output(['git',*a],cwd=root)
def sha(b):return hashlib.sha256(b).hexdigest()
require(sys.flags.optimize==0 and os.environ.get('PYTHONOPTIMIZE')=='0','explicit nonoptimized execution required')
base='6e3915b70a71657a08f0028b3f70d6074116206a';head='3d4a401ed8959befad0b2f10db59093725f27432'
require(git('rev-parse','HEAD').decode().strip()==head,'head changed')
require(not git('status','--porcelain','--untracked-files=no'),'tracked tree changed')
b=(p/'SOURCE-MANIFEST.json').read_bytes();require(sha(b)=='dfd21968df3b73ba28a7388f3504b8891e3f13035dab6f364e69fbe2947d1211','manifest changed');manifest=json.loads(b);entries={}
for row in git('ls-tree','-rz','--full-tree',head).split(b'\0'):
 if not row:continue
 fields,name=row.split(b'\t',1);mode,kind,oid=fields.decode().split();entries[name.decode()]={'mode':mode,'kind':kind,'oid':oid}
require(set(entries)=={x['path']for x in manifest if x['path']!='Cargo.lock'},'tree/source population differs')
require(not git('ls-tree',base,'--','Cargo.lock') and subprocess.run(['git','check-ignore','-q','Cargo.lock'],cwd=root).returncode==0,'lock tracking scope changed')
blobs=[(x,entries[x['path']])for x in manifest if x['mode']!='160000' and x['path']!='Cargo.lock']
batch=subprocess.run(['git','cat-file','--batch'],cwd=root,input=('\n'.join(e['oid']for x,e in blobs)+'\n').encode(),capture_output=True,check=True).stdout;pos=0
for item,entry in blobs:
 end=batch.index(b'\n',pos);header=batch[pos:end].decode().split();size=int(header[2]);pos=end+1;body=batch[pos:pos+size];pos+=size+1
 require(entry['mode']==item['mode'] and header[0]==entry['oid'] and sha(body)==item['sha256'],'committed blob differs: '+item['path'])
 source=p/'source'/item['path'];source_bytes=os.fsencode(os.readlink(source))if item['mode']=='120000' else source.read_bytes()
 require(source_bytes==body,'frozen source changed: '+item['path'])
require(pos==len(batch),'unread blob output')
links=[]
for item in manifest:
 if item['mode']=='160000':
  old=git('ls-tree',base,'--',item['path']).decode().split();entry=entries[item['path']]
  require(entry['mode']=='160000' and entry['oid']==old[2],'gitlink changed');links.append({'path':item['path'],'oid':entry['oid']})
lock=p/'source/Cargo.lock';locksha=sha(lock.read_bytes());require(locksha=='1c09663e46bf21ad7c07eedd7821cccb72ae21f42485192649ff5473962bc856','qualified ignored lock changed')
require(git('rev-parse',head+'^{tree}').decode().strip()=='73024912837cbbeaa70b6d2173f5b596eebedcb3','tree mismatch')
body=(p/'pr-body-v1.md').read_bytes();view=json.loads((p/'pr-created-view.json').read_bytes());require(view['headRefOid']==head and view['body'].encode()==body,'retained remote body differs')
proof={'base':base,'head':head,'tree':'73024912837cbbeaa70b6d2173f5b596eebedcb3','manifest_entries':len(manifest),'git_entries':len(entries),'all_tracked_source_matches':True,'gitlinks':links,'additional_ignored_lock_sha256':locksha,'lock_bytes':lock.stat().st_size,'assertions_used':False,'python_optimization':sys.flags.optimize,'pythonoptimize_environment':os.environ.get('PYTHONOPTIMIZE'),'script_sha256':sha(Path(__file__).read_bytes()),'qualification_results_sha256':sha((p/'qualification-result-v2/RESULTS.json').read_bytes()),'previous_proof_preserved':'COMMITTED-SOURCE-PROOF-v2.json','reason':'Record interpreter state and use explicit checks; earlier root proof omitted optimization state.'}
with (p/'COMMITTED-SOURCE-PROOF-v3.json').open('x')as f:json.dump(proof,f,indent=2);f.write('\n')
print(json.dumps(proof,indent=2))
