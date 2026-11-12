from pathlib import Path
import json,subprocess,hashlib,stat,shutil
D=Path(__file__).resolve().parent;R='/home/newton/work/dev-hermit/worktrees/slots/kvm-prejoin-main-20260917';B='8051335e87104f7cf832204f74920d38416393b2';H='5bc8dfd9a2c024426aaefe504f250a71394bbb09'
h=lambda b:hashlib.sha256(b).hexdigest()
def bind(p):
 p=Path(p);b=p.read_bytes();return {'path':str(p),'bytes':len(b),'sha256':h(b)}
def put(name,value):
 data=value if isinstance(value,bytes) else (json.dumps(value,indent=2)+'\n').encode()
 with (D/name).open('xb') as f:f.write(data)
for row in json.loads((D/'PACKET-v9.json').read_text())['inputs']:assert bind(row['path'])==row
M=json.loads((D/'PREVIEW-MANIFEST-v9.json').read_text());P=D/'preview-v10';P.mkdir(exist_ok=False);rows=[];audit=[];deltas=[]
for row in M['files']:
 src=Path(row['preview_path']);data=src.read_bytes();assert len(data)==row['bytes'] and h(data)==row['sha256'];assert oct(stat.S_IMODE(src.stat().st_mode))==row['mode']
 entry=subprocess.check_output(['git','ls-tree',B,'--',row['path']],cwd=R,timeout=20).decode().strip();current=subprocess.check_output(['git','ls-tree',H,'--',row['path']],cwd=R,timeout=20).decode().strip()
 mode=entry.split()[0] if entry else '100644';currentmode=current.split()[0] if current else '100644';assert mode==currentmode
 expected=int(mode[-3:],8);p=P/row['path'];p.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(src,p);p.chmod(expected)
 assert p.read_bytes()==data
 updated=dict(row,preview_path=str(p),mode=oct(expected),expected_git_mode=mode);rows.append(updated)
 audit.append({'path':row['path'],'base_git_mode':entry.split()[0] if entry else None,'current_git_mode':current.split()[0] if current else None,'expected_patch_output_mode':mode,'old_preview_mode':row['mode'],'new_preview_mode':oct(stat.S_IMODE(p.stat().st_mode)),'text_sha256':h(data),'text_unchanged':True,'mode_matches':True})
 if row['mode']!=updated['mode']:deltas.append({'path':row['path'],'old_preview_mode':row['mode'],'new_preview_mode':updated['mode'],'product_mode_change':False})
assert deltas==[{'path':'ci/configure-build-jobs.sh','old_preview_mode':'0o755','new_preview_mode':'0o644','product_mode_change':False}],deltas
patch=(D/'candidate-v9.patch').read_bytes();assert not any(line.startswith((b'old mode ',b'new mode ',b'new file mode ',b'deleted file mode ')) for line in patch.splitlines())
put('candidate-v10.patch',patch);put('v9-to-v10.patch',b'');put('BASE-v10.json',(D/'BASE-v9.json').read_bytes())
M.update(files=rows,patch=bind(D/'candidate-v10.patch'),scope='owned preview with corrected filesystem mode; source text unchanged from v9; uncompiled/unexecuted/unlanded')
put('PREVIEW-MANIFEST-v10.json',M);put('MODE-AUDIT-v10.json',{'scope':'all preview modes checked against immutable 805 and 5bc Git modes; patch has no mode headers; new helper uses regular-file 100644','files':audit,'deltas':deltas})
put('INTEGRITY-v10.json',{'scope':'mode-only successor; original exact hunk proof retained, all source bytes unchanged','prior_integrity':bind(D/'INTEGRITY-v9.json'),'prior_patch':bind(D/'candidate-v9.patch'),'new_patch':bind(D/'candidate-v10.patch'),'prior_v9_packet_preserved':True,'all38_source_bytes_unchanged':True,'all38_modes_match_git_and_patch_semantics':True,'only_preview_mode_delta':deltas,'no_product_mode_change':True,'source_declarations_and_control_logic_unchanged':True,'no_product_execution':True})
put('AFFECTED-SOURCE-TEST-NAMES-v10.json',{'scope':'unchanged v9 source declarations; no new inventory or execution','same_population':bind(D/'AFFECTED-SOURCE-TEST-NAMES-v9.json')})
print(json.dumps({'patch':bind(D/'candidate-v10.patch'),'manifest':bind(D/'PREVIEW-MANIFEST-v10.json'),'mode_audit':bind(D/'MODE-AUDIT-v10.json'),'deltas':deltas},indent=2))
