#!/usr/bin/env python3
import hashlib,json,pathlib,re,stat
P=pathlib.Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-proc-fd-identity-20260917/ignored/m2-preliminary-evidence-review-20260917')
O=pathlib.Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-prejoin-main-20260917/ignored')
def rec(p):
 p=pathlib.Path(p);s=p.stat();return dict(path=str(p),bytes=s.st_size,mode=stat.S_IMODE(s.st_mode),sha256=hashlib.sha256(p.read_bytes()).hexdigest())
def load(p):return json.loads(pathlib.Path(p).read_text())
compile=load(P/'COMPILE-LISTS-READBACK.json');child=P/'native-crosscheck';native=load(child/'AUDIT.json');checks=[]
def ck(n,v):checks.append(dict(check=n,ok=bool(v)))
for expected in load(child/'PACKET.json'):
 actual=rec(child/expected['path']);ck('child packet '+expected['path'],all(actual[k]==expected[k] for k in ('bytes','sha256')))
ck('child no audit discrepancies',native['retained_link_failures']==native['failed_audit_checks']==[])
ck('compile/list no discrepancies',compile['failed']==0)
lookup={x['artifact']:x for x in compile['inventories']}
rows=[];evidence=[]
for row,key in zip(native['rows'],['detcore','hermit','hermit-bin']):
 ck(row['phase']+' actual ELF linkage',row['runtime_executable_record']==compile['artifacts'][key]['retained'])
 ck(row['phase']+' actual emitted inventory subset',set(row['selected_names']).issubset(lookup[key]['names']))
 ck(row['phase']+' frozen source linkage',row['source_record']==compile['source_manifest'])
 base=O/'m2-controls-v1';rd=base/'controls-run-1'/row['phase']/'result.json';od=base/'observer'/row['phase']
 result=load(rd);stream=(od/'stdout').read_text();stderr=(od/'stderr').read_text()
 actual=re.findall(r'^test (.+) \.\.\. (ok|FAILED|ignored)$',stream,re.M)
 ck(row['phase']+' actual raw selected order',[a for a,b in actual]==row['selected_names'])
 ck(row['phase']+' actual raw outcomes match audit',actual==[(x['name'],x['outcome']) for x in row['actual_names_and_outcomes']])
 rows.append({k:row[k] for k in ('phase','counts','summary','raw_status','accepted','terminal_authenticated','comparison_eligible','cpu_seconds','observed_wall_seconds','service')})
 rows[-1].update(result=rec(rd),raw_stdout=rec(od/'stdout'),raw_stderr=rec(od/'stderr'),artifact=key)
 evidence.extend([rec(rd),rec(od/'stdout'),rec(od/'stderr')])
failed=rows[-1]
ck('original CLI cohort remains failed',failed['raw_status']==101 and failed['accepted'] is False and failed['comparison_eligible'] is False and failed['terminal_authenticated'] is True)
ck('exact original panic retained','left: String("info")' in stderr and 'right: "deterministic"' in stderr and 'hermit-cli/src/bin/hermit/logdiff.rs:1337:9' in stderr)
for prefix in ('logdiff_report::','analyze::phases::'):
 ck('no actual direct unit identity for '+prefix,not any(n.startswith(prefix) for inv in compile['inventories'] for n in inv['names']))
# Only the one relevant preserved source copy is read, not the evolving checkout.
copy=pathlib.Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-proc-fd-identity-20260917/ignored/m2-next-increment-20260917/composition-v10-5bc8/changed/hermit-cli/src/bin/hermit/logdiff.rs')
manifest=load(compile['source_manifest']['path']);item=next(x for x in manifest if x['path']=='hermit-cli/src/bin/hermit/logdiff.rs');frozen=rec(copy)
ck('failing source exact frozen manifest',frozen['sha256']==item['sha256'] and oct(frozen['mode'])=='0o644')
ck('failing source assertion unchanged','assert_eq!(value["comparison"]["stream"], "deterministic");' in copy.read_text())
result=dict(scope='Independent retained preliminary evidence audit; confirms failure as failure, not a product approval',checks=checks,failures=[x for x in checks if not x['ok']],compile_lists=rec(P/'COMPILE-LISTS-READBACK.json'),native_crosscheck=rec(child/'AUDIT.json'),native_packet=rec(child/'PACKET.json'),source_manifest=compile['source_manifest'],source_copy=frozen,source_base=compile['source_base'],source_tree=compile['source_tree'],native_results=rows,original_evidence=evidence)
with (P/'RESULT.json').open('x') as f:json.dump(result,f,indent=2);f.write('\n')
print(json.dumps(dict(checks=len(checks),failures=result['failures'],result=rec(P/'RESULT.json'),compile_readback=rec(P/'COMPILE-LISTS-READBACK.json')),indent=2))
