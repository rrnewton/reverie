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
 require(type(controls[0]['value'].get('test_count')) is int,'suite start count must be an integer, not a bool or float')
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
