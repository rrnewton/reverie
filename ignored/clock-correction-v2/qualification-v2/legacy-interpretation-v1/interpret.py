#!/usr/bin/python3
"""Offline, narrowly scoped legacy-output interpretation. Never runs the product."""
import datetime
import hashlib
import json
import math
import os
import re
import stat
import sys
from pathlib import Path

D=Path(__file__).resolve().parent;Q=D.parent;W=Q.parent;R=W/'v2-fix/source'
NAME='suppressed_legacy_vsyscall_returns_to_caller'
SGR=re.compile(rb'\x1b\[(?:0|1|2|3|32|34|35)m')
PREFIX=re.compile(r'(?P<timestamp>\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{6}Z) +(?P<level>DEBUG|TRACE|INFO) +(?P<context>.+): (?P<target>reverie_ptrace::(?:timer|task|vdso|syscall)): (?P<message>.+)')
TIME=re.compile(r'syscall\.intercept\{tid=(\d+) syscall=time args=SyscallArgs \{ arg0: \d+, arg1: \d+, arg2: \d+, arg3: \d+, arg4: \d+, arg5: \d+ \}\}')

def require(value,message):
 if not value:raise RuntimeError(message)
def load(p):return json.loads(Path(p).read_text())
def rec(p):
 p=Path(p);s=p.lstat();require(stat.S_ISREG(s.st_mode),'not regular '+str(p));h=hashlib.sha256()
 with p.open('rb') as f:
  while b:=f.read(1024**2):h.update(b)
 return dict(path=str(p),bytes=s.st_size,mode=stat.S_IMODE(s.st_mode),sha256=h.hexdigest())
def write(p,v):
 with p.open('x') as f:json.dump(v,f,indent=2);f.write('\n');f.flush();os.fsync(f.fileno())
def unique(pairs):
 d={}
 for k,v in pairs:require(k not in d,'duplicate JSON key '+k);d[k]=v
 return d
def bad_constant(value):raise RuntimeError('nonfinite JSON number '+value)

def interpret(stdout,stderr,status):
 require(status==0,'nonzero actual payload status')
 require(stderr==b'','unexpected stderr, including any skip marker')
 require(b'skipping legacy-vsyscall test' not in stdout+stderr,'legacy path skipped')
 require(stdout and stdout.endswith(b'\n'),'incomplete output record')
 records=[];controls=[];traces=[];offset=0
 for number,raw in enumerate(stdout.splitlines(keepends=True),1):
  require(raw.endswith(b'\n') and b'\r' not in raw,'non-LF record or carriage return')
  line=raw[:-1]
  require(line and b'\x00' not in line,'empty/NUL record')
  entry=dict(line=number,offset=offset,bytes=len(raw),sha256=hashlib.sha256(raw).hexdigest());offset+=len(raw)
  if line.lstrip().startswith((b'{',b'[')):
   require(b'\x1b' not in line,'ANSI control bytes in a JSON record')
   value=json.loads(line.decode('utf-8'),object_pairs_hook=unique,parse_constant=bad_constant)
   require(isinstance(value,dict),'control record is not an object');entry.update(kind='libtest',value=value);controls.append(entry)
  else:
   require(line.startswith(b'\x1b[2m'),'unrecognized non-JSON record')
   plain=SGR.sub(b'',line)
   require(all(32<=b<127 for b in plain),'unknown ANSI/control/non-ASCII tracing bytes')
   match=PREFIX.fullmatch(plain.decode('ascii'));require(match is not None,'tracing prefix/context/target mismatch')
   fields=match.groupdict();datetime.datetime.fromisoformat(fields['timestamp'][:-1]+'+00:00')
   entry.update(kind='tracing',plain=plain.decode('ascii'),**fields);traces.append(entry)
  records.append(entry)
 require(offset==len(stdout),'not all bytes classified')
 require(len(controls)==4,'exactly four libtest lifecycle records required')
 expected=[{'type':'suite','event':'started','test_count':1},{'type':'test','event':'started','name':NAME},{'type':'test','name':NAME,'event':'ok'}]
 require([x['value'] for x in controls[:3]]==expected,'started/name/outcome lifecycle mismatch')
 summary=controls[3]['value'];require(set(summary)=={'type','event','passed','failed','ignored','measured','filtered_out','exec_time'},'suite fields differ')
 require(summary['type']=='suite' and summary['event']=='ok','suite did not pass')
 for key,want in [('passed',1),('failed',0),('ignored',0),('measured',0),('filtered_out',0)]:
  require(type(summary[key]) is int and summary[key]==want,'suite count differs: '+key)
 require(type(summary['exec_time']) in (int,float) and math.isfinite(summary['exec_time']) and summary['exec_time']>=0,'invalid elapsed value')
 require(controls[0]['line']==1 and controls[1]['line']==2 and controls[3]['line']==len(records),'extra output before/after lifecycle')
 require(traces and all(controls[1]['line']<x['line']<controls[2]['line'] for x in traces),'tracing outside running exact test')
 time_records=[x for x in traces if x['target']=='reverie_ptrace::syscall' and TIME.fullmatch(x['context'])]
 require(len(time_records)==2 and [x['message'] for x in time_records]==['intercepting guest syscall','completed guest syscall interception'],'real time interception pair absent')
 require(time_records[0]['context']==time_records[1]['context'],'time task/arguments changed within pair')
 return dict(records=records,control_lines=[x['line'] for x in controls],trace_lines=[x['line'] for x in traces],time_interception_lines=[x['line'] for x in time_records],time_context=time_records[0]['context'],summary=summary,total_bytes=len(stdout),raw_status=status,all_bytes_classified=True,qualified_original_receipt=False)

require(sys.flags.optimize==0,'unoptimized offline reader required')
before=load(Q/'final-v1/INPUTS.json');files={};aliases={}
for row in before['files']:
 require(rec(row['path'])==row,'frozen input drift: '+row['path']);files[row['path']]=row
for row in before['symlinks']:
 require(Path(row['path']).is_symlink() and os.readlink(row['path'])==row['target'],'frozen alias drift');aliases[row['path']]=row
receipt=load(Q/'controls/test-38/result.json');observed=load(Q/'observer/test-38/result.json')
require(receipt['accepted'] is False and receipt['raw_status']==0 and receipt['terminal_authenticated'] and receipt['inputs_unchanged'],'original refusal changed')
require(receipt['readback_error'].startswith('JSONDecodeError(') and observed['accounting_complete'],'different failure or incomplete terminal')
out=(Q/'observer/test-38/stdout').read_bytes();err=(Q/'observer/test-38/stderr').read_bytes()
(D/'stdout.raw').open('xb').write(out);(D/'stderr.raw').open('xb').write(err)
positive=interpret(out,err,receipt['raw_status']);write(D/'INTERPRETATION.json',positive)
# Evidence corruption probes reuse the entire actual stream, changing one stated obligation.
# They are offline reader probes, not product tests, and contribute no runtime denominator.
lines=out.splitlines(keepends=True);end=positive['control_lines'][2]-1;suite=positive['control_lines'][3]-1
def replace_line(index,data):return b''.join(lines[:index]+[data]+lines[index+1:])
cases=[]
cases.append(('malformed_json_control',replace_line(end,b'{"type":"test",\n'),err,0))
cases.append(('duplicate_start',b''.join(lines[:end]+[lines[1]]+lines[end:]),err,0))
cases.append(('ignored_outcome',replace_line(end,lines[end].replace(b'"ok"',b'"ignored"')),err,0))
cases.append(('wrong_test_name',replace_line(end,lines[end].replace(NAME.encode(),(NAME+'_other').encode())),err,0))
cases.append(('mismatched_suite_count',replace_line(suite,lines[suite].replace(b'"passed": 1',b'"passed": 2')),err,0))
cases.append(('ansi_wrapped_malformed_control',replace_line(end,b'\x1b[2m{"type":"test",\x1b[0m\n'),err,0))
cases.append(('duplicate_terminal_record',out+lines[-1],err,0))
cases.append(('missing_suite_completion',b''.join(lines[:-1]),err,0))
cases.append(('unframed_diagnostic',b''.join(lines[:end]+[b'otherwise ignored warning\n']+lines[end:]),err,0))
cases.append(('cursor_control_in_trace',replace_line(2,b'\x1b[2J'+lines[2]),err,0))
cases.append(('missing_time_effect',out.replace(b'syscall\x1b[0m\x1b[2m=\x1b[0mtime ',b'syscall\x1b[0m\x1b[2m=\x1b[0mgetpid '),err,0))
cases.append(('nonzero_raw_status',out,err,101))
cases.append(('skip_marker_in_stderr',out,b'skipping legacy-vsyscall test: host has no [vsyscall] mapping\n',0))
cases.append(('duplicate_json_event',replace_line(end,b'{"type":"test","name":"'+NAME.encode()+b'","event":"failed","event":"ok"}\n'),err,0))
control_dir=D/'negative-transcripts';control_dir.mkdir(mode=0o700);negatives=[]
for name,stdout,stderr,status in cases:
 require(stdout!=out or stderr!=err or status!=0,'negative mutation was ineffective: '+name)
 op=control_dir/(name+'.stdout');ep=control_dir/(name+'.stderr');op.open('xb').write(stdout);ep.open('xb').write(stderr)
 try:interpret(stdout,stderr,status)
 except (RuntimeError,ValueError,UnicodeError) as error:reason=repr(error)
 else:raise RuntimeError('corrupt evidence accepted: '+name)
 negatives.append(dict(name=name,stdout=rec(op),stderr=rec(ep),raw_status=status,refused=True,reason=reason,synthetic_offline_evidence=True))
write(D/'NEGATIVE-CONTROLS.json',negatives)
source_paths=[R/'reverie-ptrace/tests/vsyscall.rs',R/'reverie-ptrace/src/testing.rs',Path('/home/newton/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tracing-subscriber-0.3.23/src/fmt/mod.rs'),Path('/home/newton/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tracing-subscriber-0.3.23/src/fmt/fmt_layer.rs')]
write(D/'SOURCE-BINDING.json',dict(files=[rec(p) for p in source_paths],exact_test=NAME,writer='fmt_layer.rs:749 default make_writer io::stdout',forced_filter='testing.rs:50-52 with_env_filter("reverie=trace")',guest_success_gate='testing.rs:132-138 requires ExitStatus::Exited(0)',test_assertions='vsyscall.rs:66-68 requires returned and stored FIXED_TIME',retained_elf=load(Q/'retained-binaries/ARTIFACTS.json')['vsyscall']['file'],compiled_binding=rec(Q/'retained-binaries/BINDING.json')))
report=f'''# Offline interpretation of the legacy-vsyscall stream

A narrowly specified tracing-prefix-aware reader classifies the retained run as one exact test that completed successfully: one suite start (count1), one matching test start, one matching test ok and one suite ok (passed1, failed/ignored/measured/filtered0). Those four records occur at lines {positive['control_lines']}. The other {len(positive['trace_lines'])} lines are separately classified ANSI tracing records. All {len(out)} stdout bytes and {len(err)} stderr bytes are retained verbatim; byte offsets, lengths and hashes reconstruct the entire original stream. No unknown line is discarded.

The original qualification remains raw0 / accepted=false with JSONDecodeError, and Q2 remains44 accepted phases /37 qualified declarations. This is a separate offline interpretation, not a rewritten receipt or product rerun. Root and independent reviewers decide what credit this supplementary evidence warrants.

Actual legacy execution is visible: lines {positive['time_interception_lines']} record start/completion of the same `time` syscall context, {positive['time_context']}. The exact source checks both the returned value and stored value against FIXED_TIME. The shared testing helper requires child Exited(0), and the emitted exact test/suite ok records follow the actual time interception. Stderr is empty, and neither stream contains the legacy skip marker. This is more than a bare process exit status.

The stdout interleaving is source-derived: testing.rs forces a `reverie=trace` subscriber; the bound tracing-subscriber default writer is stdout. Setting RUST_LOG=off would not replace the explicit filter. No logging configuration or test source was changed here.

The proposed interpretation has two explicit grammars. JSON-looking lines must parse completely with unique keys, finite values, exact lifecycle/name/counts and no ANSI control bytes. The only admitted non-JSON records begin with the actual dim ANSI timestamp prefix; after removing only the known SGR decoration, they must have a valid timestamp, DEBUG/TRACE/INFO level, context and one of the four bound reverie_ptrace targets. Unrecognized ANSI, cursor controls, arbitrary output, malformed JSON, duplicate records and missing completions refuse. The legacy-specific interpretation additionally requires the matching real time-interception pair, empty stderr, no skip and raw status zero. It is intentionally limited to this retained stream format, not a universal tracing or libtest parser.

Fourteen offline corruption probes were refused: malformed JSON, duplicate start, ignored outcome, wrong name, wrong suite count, ANSI-wrapped malformed control, duplicate terminal record, missing completion, unframed diagnostic, cursor-control trace, missing time effect, nonzero raw status, skip marker and duplicate JSON key. Each changes the stated evidence obligation while retaining the surrounding original stream. They are not product tests, guest executions or additions to the38-declaration denominator. No synthetic old-algorithm variant was built.

Goalpost assessment: assertion values, exact test identity, counts, error/skip handling and raw process status are retained. No malformed control is accepted as harmless tracing. The original reader's JSON-only transport assumption is the identified limitation; its refusal remains visible. The interpretation separates authenticated diagnostic records while checking the same test outcome obligations and a stronger actual-path condition. This author analysis is not independent source approval.

Liveness: the collected Q2 driver exited1. All45 phase services have terminal accounting and normal lease completion. The separately retained all-UID census found no live Q2 caller or retained test ELF among3,808 readable processes, with no unreadable entries. No later product/caller launch occurred; no Q2 SCM guard remains live. Only this offline reader ran.
'''
(D/'REPORT.md').open('x').write(report)
for p in [*source_paths,Q/'final-v1/TARGET.json',Q/'final-v1/READBACK.json',Q/'final-v1/INPUTS.json']:
 files[str(p)]=rec(p)
for p in D.rglob('*'):
 if p.is_file():files[str(p)]=rec(p)
write(D/'INPUTS.json',dict(files=sorted(files.values(),key=lambda x:x['path']),symlinks=sorted(aliases.values(),key=lambda x:x['path']),prior_mutable_targets_and_lease_files_excluded=True))
write(D/'TARGET.json',dict(scope='Offline legacy transcript interpretation; original refusal unchanged; no execution or independent approval',report=rec(D/'REPORT.md'),interpretation=rec(D/'INTERPRETATION.json'),negative_controls=rec(D/'NEGATIVE-CONTROLS.json'),source_binding=rec(D/'SOURCE-BINDING.json'),liveness=rec(D/'LIVENESS.json'),inputs=rec(D/'INPUTS.json'),original_receipt=rec(Q/'controls/test-38/result.json')))
write(D/'READBACK.json',dict(target=rec(D/'TARGET.json'),report=rec(D/'REPORT.md'),inputs=rec(D/'INPUTS.json'),regular_files=len(files),literal_aliases=len(aliases),original_bytes_unchanged=True,original_refusal_unchanged=True,product_executions=0,original_json_records=4,original_trace_records=26,corruption_probes_refused=len(negatives),all_bytes_classified=True))
print(json.dumps(dict(target=rec(D/'TARGET.json'),report=rec(D/'REPORT.md'),readback=rec(D/'READBACK.json'),files=len(files),aliases=len(aliases),negative_controls=len(negatives))))
