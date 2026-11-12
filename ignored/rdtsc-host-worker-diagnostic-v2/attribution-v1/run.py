from pathlib import Path
import csv,hashlib,json,os,re,struct,sys
N=Path(__file__).resolve().parent;D=N.parent;Q=D/'qualification-v1';F=Q/'fixture-evidence';sys.path.insert(0,str(Q))
from common import bounded_process,file_record,write_new,require
assert sys.flags.optimize==0 and __debug__
result=json.loads((Q/'controls/host-worker-diagnostic/result.json').read_text());require(result['raw_status']==101 and not result['accepted'] and result['terminal_authenticated'] and result['inputs_unchanged'],'actual diagnostic failure not authenticated')
inputs=[file_record(p)for p in sorted(F.iterdir())]+[file_record(Q/'controls/host-worker-diagnostic/result.json'),file_record(Q/'host-worker-diagnostic-plan.json')]
for rel in ['reverie-kvm/src/elf.rs','reverie-kvm/src/runtime.rs','reverie-kvm/tests/static_elf.rs']:inputs.append(file_record(D/'source'/rel))
env={'PATH':'/usr/bin:/bin','LC_ALL':'C','LANG':'C','TZ':'UTC'}
programs=['/usr/bin/prlimit','/usr/bin/readelf','/usr/bin/objdump','/usr/bin/ldd'];links=[]
for i,name in enumerate(programs):
 p=Path(name)
 for a in [p,*p.parents]:
  if a.is_symlink():links.append(dict(path=str(a),target=os.readlink(a)))
 actual=p.resolve(strict=True);inputs.append(file_record(actual))
 r=bounded_process(['/usr/bin/ldd',str(actual)],N,env,N/('loader-'+str(i)),5,65536);require(r['returncode']==0 and not r['forced'],'query loader failed');require(Path(r['stdout']['path']).stat().st_size<=65536 and Path(r['stderr']['path']).stat().st_size<=65536,'query loader cap exceeded');inputs.extend([r['stdout'],r['stderr']])
 for text in Path(r['stdout']['path']).read_text().splitlines():
  require('not found'not in text,'query loader missing')
  for n in re.findall(r'(/[^\s()]+)',text):
   q=Path(n)
   for a in [q,*q.parents]:
    if a.is_symlink():links.append(dict(path=str(a),target=os.readlink(a)))
   inputs.append(file_record(q.resolve(strict=True)))
write_new(N/'INPUTS-BEFORE.json',dict(records=list({r['path']:r for r in inputs}.values()),symlinks=list({r['path']:r for r in links}.values())))
queries=[]
for label,filename in [('main','timestamp-host-worker.elf'),('interpreter','interpreter.elf')]:
 for kind,args in [('readelf',['/usr/bin/readelf','-W','-h','-l','-s']),('disassembly',['/usr/bin/objdump','-d'])]:
  argv=['/usr/bin/prlimit','--cpu=30:30','--fsize=16777216:16777216','--',*args,str(F/filename)]
  r=bounded_process(argv,N,env,N/(label+'-'+kind),60,16777216);queries.append(r)
  require(r['returncode']==0 and not r['forced'],'ELF query failed');require(Path(r['stdout']['path']).stat().st_size<=16777216 and Path(r['stderr']['path']).stat().st_size<=16777216,'ELF query final output cap exceeded')
write_new(N/'QUERIES.json',queries)
def elf(name):
 b=(F/name).read_bytes();require(b[:6]==b'\x7fELF\x02\x01','expected little-endian ELF64');typ,machine=struct.unpack_from('<HH',b,16);require(machine==62,'not x86-64');phoff=struct.unpack_from('<Q',b,32)[0];size,count=struct.unpack_from('<HH',b,54);require(size==56,'program header shape');ph=[struct.unpack_from('<IIQQQQQQ',b,phoff+i*size)for i in range(count)];return b,typ,ph
main,typ,mainph=elf('timestamp-host-worker.elf');mainbias=0 if typ==2 else 2*1024*1024;require(typ in (2,3),'unsupported main type');mainend=max(mainbias+p[3]+p[6]for p in mainph if p[0]==1);require(mainend<=16*1024*1024,'not the preserved small-main interpreter placement');interp,itype,iph=elf('interpreter.elf');require(itype==3,'interpreter not DYN');interpbias=16*1024*1024
callbacks=list(csv.DictReader((F/'callbacks.tsv').open(),delimiter='\t'));require(callbacks and list(callbacks[0])==['pid','instruction','rip'],'callback fields differ')
roles=[];contexts=[]
for index,row in enumerate(callbacks):
 rip=int(row['rip'],16);matches=[]
 for label,data,ph,bias in [('main',main,mainph,mainbias),('interpreter',interp,iph,interpbias)]:
  for p in ph:
   if p[0]==1 and p[1]&1 and bias+p[3]<=rip<bias+p[3]+p[5]:
    offset=p[2]+rip-(bias+p[3]);instruction=data[offset:offset+2];require(instruction==b'\x0f\x31','saved callback address not actual retained RDTSC bytes');matches.append(dict(ordinal=index+1,sender_tid=int(row['pid']),request=row['instruction'],rip=hex(rip),image=label,load_bias=hex(bias),elf_vaddr=hex(rip-bias),file_offset=offset,bytes=instruction.hex()))
 require(len(matches)==1,'ambiguous or absent executable mapping');match=matches[0];roles.append(match)
 text=(N/(match['image']+'-disassembly.stdout')).read_text().splitlines();pattern=re.compile(r'^\s*'+format(rip-int(match['load_bias'],16),'x')+r':\s');found=[i for i,l in enumerate(text)if pattern.search(l)];require(len(found)==1,'saved RIP not a disassembly boundary');i=found[0];require('rdtsc' in text[i],'disassembly not rdtsc');contexts.append(dict(callback=match,lines=text[max(0,i-4):i+5]))
write_new(N/'ATTRIBUTION.json',dict(callbacks=roles,main_elf_type=typ,main_bias=hex(mainbias),main_loaded_end=hex(mainend),interpreter_bias=hex(interpbias),method='Actual callback RIP matched to retained executable PT_LOAD bytes plus objdump instruction boundaries; biases are from exact frozen elf.rs loader source. No new guest was executed by these queries.',original_test_result='failed raw101; exact expected two was not changed'))
write_new(N/'CONTEXTS.json',contexts)
for r in inputs:require(file_record(r['path'])==r,'attribution input changed')
for r in links:require(os.readlink(r['path'])==r['target'],'attribution alias changed')
write_new(N/'READBACK.json',dict(inputs=file_record(N/'INPUTS-BEFORE.json'),queries=file_record(N/'QUERIES.json'),attribution=file_record(N/'ATTRIBUTION.json'),contexts=file_record(N/'CONTEXTS.json'),unchanged=True,guest_executed=False))
print(json.dumps(dict(callback_count=len(roles),images={label:sum(r['image']==label for r in roles)for label in ['main','interpreter']},readback=file_record(N/'READBACK.json')),indent=2))
