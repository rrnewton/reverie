from pathlib import Path
import hashlib,json,subprocess,resource,time,os,signal
D=Path(__file__).resolve().parent; F=D/'format-v9'; F.mkdir(exist_ok=False)
formatter=Path('/home/newton/.rustup/toolchains/nightly-x86_64-unknown-linux-gnu/bin/rustfmt')
h=lambda b:hashlib.sha256(b).hexdigest()
assert h(formatter.read_bytes())=='016e3712bb170692190f20ce3356646d6d2c59343b81516de66af7c5fd2e5e7c'
regions={
 'scripts/lib/validate_cell_results.rs':[
  ('    #[test]\n    fn matched_projection_rejects_unequal_counts_and_retains_sticky_divergence()', '    #[test]\n    fn schema7_binds_virtual_time_to_the_comparison_mode()')],
 'hermit-cli/tests/verification_report_consumers.rs':[
  ('#[test]\nfn qemu_boot_consumer_rejects_noncanonical_and_unequal_match_claims()', '#[test]\nfn json_output_retains_failure_evidence_without_satisfying_a_match_requirement()')]
}
rows=[]
def bounds():
 resource.setrlimit(resource.RLIMIT_CPU,(10,10));resource.setrlimit(resource.RLIMIT_FSIZE,(1048576,1048576));os.setsid()
for path,sections in regions.items():
 p=D/'preview-v9'/path; before=p.read_bytes(); temp=F/'formatted'/path; temp.parent.mkdir(parents=True,exist_ok=True);temp.write_bytes(before)
 backup=F/'before'/path; backup.parent.mkdir(parents=True,exist_ok=True); backup.write_bytes(before)
 out=F/(path.replace('/','_')+'.stdout');err=F/(path.replace('/','_')+'.stderr')
 argv=[str(formatter),'--edition','2024','--config','skip_children=true',str(temp)]
 t=time.monotonic(); c=resource.getrusage(resource.RUSAGE_CHILDREN); timedout=False
 with out.open('xb') as so,err.open('xb') as se:
  proc=subprocess.Popen(argv,stdout=so,stderr=se,preexec_fn=bounds)
  try:rc=proc.wait(timeout=20)
  except subprocess.TimeoutExpired:
   timedout=True;os.killpg(proc.pid,signal.SIGKILL);rc=proc.wait()
 after_c=resource.getrusage(resource.RUSAGE_CHILDREN)
 row={'path':path,'argv':argv,'status':rc,'timeout':timedout,'wall_seconds':time.monotonic()-t,'cpu_seconds':after_c.ru_utime+after_c.ru_stime-c.ru_utime-c.ru_stime,'before_sha256':h(before),'formatted_copy_sha256':h(temp.read_bytes()),'stdout':str(out),'stderr':str(err)}
 rows.append(row)
 (F/'RESULT.json').write_text(json.dumps({'scope':'standalone owned-preview formatting; only changed methods copied back; no product build/test/guest execution','formatter':str(formatter),'formatter_sha256':h(formatter.read_bytes()),'cpu_seconds_per_file':10,'wall_seconds_per_file':20,'output_bytes_cap':1048576,'files':rows},indent=2)+'\n')
 assert not timedout and rc==0,row
 current=before.decode(); formatted=temp.read_text()
 for start,end in sections:
  assert current.count(start)==1 and formatted.count(start)==1
  a=current.index(start);b=current.index(end,a);fa=formatted.index(start);fb=formatted.index(end,fa)
  current=current[:a]+formatted[fa:fb]+current[b:]
 p.write_text(current);row['preview_after_sha256']=h(p.read_bytes())
 (F/'RESULT.json').write_text(json.dumps({'scope':'standalone owned-preview formatting; only changed methods copied back; no product build/test/guest execution','formatter':str(formatter),'formatter_sha256':h(formatter.read_bytes()),'cpu_seconds_per_file':10,'wall_seconds_per_file':20,'output_bytes_cap':1048576,'files':rows},indent=2)+'\n')
print(json.dumps(rows,indent=2))
