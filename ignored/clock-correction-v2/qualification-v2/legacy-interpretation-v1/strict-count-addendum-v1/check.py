#!/usr/bin/python3
"""Offline evidence probes only; no product command or original receipt changes."""
import importlib.util,json,sys
from pathlib import Path
A=Path(__file__).resolve().parent;D=A.parent
spec=importlib.util.spec_from_file_location('held_narrow_reader',A/'narrow_reader.py');m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
m.require(sys.flags.optimize==0,'unoptimized reader required')
out=(D/'stdout.raw').read_bytes();err=(D/'stderr.raw').read_bytes()
positive=m.interpret(out,err,0);m.write(A/'INTERPRETATION.json',positive)
rows=[]
for control in m.load(D/'NEGATIVE-CONTROLS.json'):
 for key in ['stdout','stderr']:m.require(m.rec(control[key]['path'])==control[key],'held negative bytes changed')
 try:m.interpret(Path(control['stdout']['path']).read_bytes(),Path(control['stderr']['path']).read_bytes(),control['raw_status'])
 except (RuntimeError,ValueError,UnicodeError) as e:rows.append(dict(name=control['name'],refused=True,error=repr(e)))
 else:raise RuntimeError('corrupt transcript accepted')
for name,value in [('boolean_count',b'true'),('floating_count',b'1.0')]:
 m.require(out.count(b'"test_count": 1')==1,'count mutation ambiguous');corrupt=out.replace(b'"test_count": 1',b'"test_count": '+value);p=A/(name+'.stdout');p.open('xb').write(corrupt)
 try:m.interpret(corrupt,err,0)
 except (RuntimeError,ValueError,UnicodeError) as e:rows.append(dict(name=name,stdout=m.rec(p),refused=True,error=repr(e)))
 else:raise RuntimeError('incorrect count type accepted')
m.write(A/'NEGATIVE-CONTROLS.json',rows)
report='''# Count-type addendum to offline interpretation

The retained stream's exact integer count1 remains unchanged and the strict reader still identifies four libtest lifecycle records,26 tracing records and the same actual time-interception pair. The original qualification remains refused; neither it nor the first offline packet is rewritten.

Author review found that Python dictionary equality in the first prototype would equate a corrupted suite-start count `true` or `1.0` with integer1. That does not change the interpretation of the actual integer1 bytes, but it is a concrete proposed-reader limitation. The append-only one-line delta now requires the count's type to be int. The original14 corruption probes and both count-type corruptions are all refused by this strict version. These16 offline probes do not add product executions or test credit.

Use this strict reader for the proposed format interpretation. It remains deliberately limited to the bound legacy transcript format and does not replace the original receipt or independently approve source. Every byte of the actual stdout/stderr is retained. No logger, runtime helper, product source, test assertion or result denominator changed.
'''
(A/'REPORT.md').open('x').write(report)
files=[m.rec(p) for p in A.iterdir() if p.is_file()]+[m.rec(D/n) for n in ['TARGET.json','READBACK.json','INPUTS.json','stdout.raw','stderr.raw','NEGATIVE-CONTROLS.json','interpret.py']]
for control in m.load(D/'NEGATIVE-CONTROLS.json'):files.extend(control[k] for k in ['stdout','stderr'])
m.write(A/'INPUTS.json',dict(files=files))
m.write(A/'TARGET.json',dict(report=m.rec(A/'REPORT.md'),interpretation=m.rec(A/'INTERPRETATION.json'),reader=m.rec(A/'narrow_reader.py'),delta=m.rec(A/'READER-DELTA.patch'),controls=m.rec(A/'NEGATIVE-CONTROLS.json'),inputs=m.rec(A/'INPUTS.json'),original_packet=m.rec(D/'TARGET.json'),product_executions=0,original_refusal_unchanged=True))
m.write(A/'READBACK.json',dict(target=m.rec(A/'TARGET.json'),report=m.rec(A/'REPORT.md'),inputs=m.rec(A/'INPUTS.json'),all16_corruptions_refused=len(rows)==16,actual_json_records=4,actual_trace_records=26,original_bytes_unchanged=out==(D/'stdout.raw').read_bytes() and err==(D/'stderr.raw').read_bytes(),product_executions=0))
print(json.dumps(dict(target=m.rec(A/'TARGET.json'),report=m.rec(A/'REPORT.md'),readback=m.rec(A/'READBACK.json'),negative_controls=len(rows))))
