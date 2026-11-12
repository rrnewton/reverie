"""Authenticate immutable source bytes/patch reconstruction, never execute product code."""
from pathlib import Path
import hashlib,json,os,re,subprocess
D=Path(__file__).resolve().parent;R=D.parents[1]
P=R/'ignored/process-publication-implementation-v1/frozen-v1'
F=R/'ignored/same-inode-ofd-repair-v2/final-v3'
paths=['reverie-kvm/src/elf.rs','reverie-kvm/src/executor.rs','reverie-kvm/src/process_signal_publication.rs']
def record(p):
 b=p.read_bytes();return {'path':str(p),'bytes':len(b),'sha256':hashlib.sha256(b).hexdigest()}
def write(name,x):
 p=D/name;assert not p.exists(),p;p.write_text(json.dumps(x,indent=2)+'\n')
def check_record(row):
 assert record(Path(row['path']))==row,row['path']
def reconstruct(text,oldroot):
 lines=text.splitlines(keepends=True);i=0;result={}
 while i<len(lines):
  assert lines[i].startswith('--- ');old=lines[i][4:].strip();i+=1
  assert lines[i].startswith('+++ b/');rel=lines[i][6:].strip();i+=1
  previous=[]if old=='/dev/null'else(oldroot/rel).read_text().splitlines(keepends=True)
  out=[];cursor=0
  while i<len(lines)and lines[i].startswith('@@ '):
   m=re.match(r'@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@',lines[i]);assert m,lines[i]
   start=int(m[1]);oc=int(m[2]or'1');nc=int(m[4]or'1');i+=1
   pos=start if oc==0 else start-1
   assert pos>=cursor,(rel,pos,cursor);out+=previous[cursor:pos];cursor=pos;a=b=0
   while i<len(lines)and not lines[i].startswith(('@@ ','--- ')):
    line=lines[i];i+=1
    if line[0] in ' -':assert cursor<len(previous)and previous[cursor]==line[1:],(rel,cursor,line);cursor+=1;a+=1
    if line[0] in ' +':out.append(line[1:]);b+=1
    assert line[0]in' +-'
   assert(a,b)==(oc,nc),(rel,a,b,oc,nc)
  out+=previous[cursor:];actual=''.join(out).encode();assert actual==(D/'source'/rel).read_bytes(),rel
  result[rel]=hashlib.sha256(actual).hexdigest()
 return result
proof={}
for name,oldroot in [('SOURCE.patch',D/'base'),('PUBLISHER-TO-COMPOSED.patch',P/'source'),('FD-TO-COMPOSED.patch',F/'after')]:
 proof[name]={'patch':record(D/name),'reconstructed_after_sha256':reconstruct((D/name).read_text(),oldroot)}
assert set(proof['SOURCE.patch']['reconstructed_after_sha256'])==set(paths)
assert set(proof['PUBLISHER-TO-COMPOSED.patch']['reconstructed_after_sha256'])==set(paths[:2])
# All initial component packet bytes and source records remain unchanged.
initial=json.loads((D/'INPUTS-INITIAL.json').read_text())
for row in initial['publisher']+initial['fd']+[initial['lock']]:check_record(row)
manifest=json.loads((D/'SOURCE-MANIFEST.json').read_text())
for row in manifest:
 p=D/'source'/row['relative']
 if row['kind']=='unexpanded_gitlink':assert p.is_dir()and not list(p.iterdir())
 elif row['kind']=='symlink':assert p.is_symlink()and os.readlink(p)==row['target']
 else:
  check_record({k:row[k]for k in ['path','bytes','sha256']})
  assert bool(p.stat().st_mode&0o111)==(row['mode']=='100755')
for rel in paths:assert(D/'after'/rel).read_bytes()==(D/'source'/rel).read_bytes()
def git(*args):
 c=subprocess.run(['git','-C',str(R),*args],stdout=subprocess.PIPE,stderr=subprocess.PIPE,env={**os.environ,'GIT_OPTIONAL_LOCKS':'0'},timeout=30)
 assert c.returncode==0,(args,c.returncode,c.stderr);return c.stdout
before=json.loads((D/'LIVE-BEFORE.json').read_text())
after={'head':git('rev-parse','HEAD').decode().strip(),'branch':git('branch','--show-current').decode().strip(),'index':record(Path(before['index']['path'])),'live':[record(R/x)for x in paths]}
assert before==after,(before,after)
write('LIVE-CONTINUITY.json',{'before':before,'after':after,'identical':True,'scope':'HEAD, branch, complete index bytes and three live publisher product files; no live writes or runtime/cache activity'})
write('RECONSTRUCTION.json',{'diffs':proof,'manifest_entries_checked':len(manifest),'all_component_input_bindings_unchanged':True,'source_only':True})
files=['REPORT.md','CONFLICTS.md','PLAN.md','SOURCE.patch','PUBLISHER-TO-COMPOSED.patch','FD-TO-COMPOSED.patch','CALLER-CHANGE.patch','SOURCE_INPUTS.json','SOURCE-MANIFEST.json','CALLER_INPUTS.json','TEST-SOURCE-CONTINUITY.json','INPUTS-INITIAL.json','LIVE-BEFORE.json','LIVE-CONTINUITY.json','AUTO-MERGE.json','RECONSTRUCTION.json','resolve.py','prepare_packet.py','freeze.py','qualification-proposal/prepare.py','qualification-proposal/SETUP.json','qualification-proposal/SELECTORS.json','qualification-proposal/source-manifest.json']
records=[record(D/x)for x in files]
records += [record(D/'after'/x)for x in paths]+[record(D/'base'/x)for x in paths[:2]]
records += [record(D/'auto-merge'/x)for x in paths[:2]]
records += [record(D/'auto-merge/reverie-kvm/src'/x)for x in ['elf.stderr','executor.stderr']]
records += [record(D/'source/Cargo.lock')]
records += [{k:row[k]for k in ['path','bytes','sha256']}for row in manifest if row['kind']=='file']
for row in records:check_record(row)
write('READBACK.json',{'records':records,'manifest':record(D/'SOURCE-MANIFEST.json'),'source_manifest_entries':len(manifest),'record_count':len(records),'component_bindings':record(D/'INPUTS-INITIAL.json'),'no_product_execution':True,'no_live_source_or_SCM_change':True})
write('TARGET.json',{'base':initial['base'],'status':'SOURCE-ONLY AUTHOR COMPOSITION; independent review and qualification pending','report':record(D/'REPORT.md'),'conflicts':record(D/'CONFLICTS.md'),'plan':record(D/'PLAN.md'),'source_patch':record(D/'SOURCE.patch'),'publisher_delta':record(D/'PUBLISHER-TO-COMPOSED.patch'),'fd_delta':record(D/'FD-TO-COMPOSED.patch'),'caller_delta':record(D/'CALLER-CHANGE.patch'),'source_inputs':record(D/'SOURCE_INPUTS.json'),'readback':record(D/'READBACK.json'),'product_paths':paths,'proposed_unique_test_declarations':73,'execution_performed':False})
print(json.dumps({'target':record(D/'TARGET.json'),'readback':record(D/'READBACK.json'),'report':record(D/'REPORT.md'),'source_patch':record(D/'SOURCE.patch'),'manifest':record(D/'SOURCE-MANIFEST.json')},indent=2))
