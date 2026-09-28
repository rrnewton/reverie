from pathlib import Path
import hashlib,json,os,platform,resource,shutil,signal,struct,subprocess,time
D=Path(__file__).resolve().parent
assert platform.machine()=='x86_64'
def rec(p):
 b=p.read_bytes();return dict(path=str(p),bytes=len(b),sha256=hashlib.sha256(b).hexdigest())
def exclusive_json(name,data):
 with (D/name).open('x') as f:f.write(json.dumps(data,indent=2)+'\n')
def run(label,argv,cpu,wall,memory,output,pass_fds=()):
 limits=dict(cpu_seconds=cpu,wall_seconds=wall,address_space_bytes=memory,file_size_bytes=output,stdout_limit_bytes=output,stderr_limit_bytes=output,core_bytes=0)
 def restrict():
  resource.setrlimit(resource.RLIMIT_CPU,(cpu,cpu))
  resource.setrlimit(resource.RLIMIT_AS,(memory,memory))
  resource.setrlimit(resource.RLIMIT_FSIZE,(output,output))
  resource.setrlimit(resource.RLIMIT_CORE,(0,0))
 out=D/(label+'.stdout');err=D/(label+'.stderr')
 before=resource.getrusage(resource.RUSAGE_CHILDREN);start=time.monotonic();timed_out=False
 with out.open('xb') as stdout,err.open('xb') as stderr:
  process=subprocess.Popen(argv,cwd=D,stdin=subprocess.DEVNULL,stdout=stdout,stderr=stderr,pass_fds=pass_fds,start_new_session=True,preexec_fn=restrict,env=dict(os.environ,TMPDIR=str(D/'temporary')))
  try:status=process.wait(timeout=wall)
  except subprocess.TimeoutExpired:
   timed_out=True;os.killpg(process.pid,signal.SIGKILL);status=process.wait(timeout=2)
 elapsed=time.monotonic()-start;after=resource.getrusage(resource.RUSAGE_CHILDREN)
 result=dict(label=label,argv=argv,limits=limits,raw_status=status,timed_out=timed_out,wall_seconds=elapsed,user_cpu_seconds=after.ru_utime-before.ru_utime,system_cpu_seconds=after.ru_stime-before.ru_stime,stdout=rec(out),stderr=rec(err),process_id=process.pid,scope='Native assembly/link or direct Linux syscall only; no Hermit/KVM execution')
 exclusive_json(label+'.json',result)
 assert status==0 and not timed_out,result
 return result
(D/'temporary').mkdir()
assembler=Path(shutil.which('as')).resolve();linker=Path(shutil.which('ld')).resolve();objdump=Path(shutil.which('objdump')).resolve()
exclusive_json('NATIVE-INPUTS.json',dict(host=dict(os.uname()._asdict()) if hasattr(os.uname(),'_asdict') else dict(zip(['sysname','nodename','release','version','machine'],os.uname())),tools=[rec(p)for p in [assembler,linker,objdump]],runner=rec(Path(__file__)),syscall_number=1,raw_fd_hex='0x100000001',argument_width_bits=64,payload_hex='616263',count=3,unused_arguments=['0x63617077',17,'0x9876'],record_format='Little-endian 9 words: syscall number, six raw arguments, argument width bits, signed raw RAX'))
raw_path=D/'native-raw-record.bin';report_fd=os.open(raw_path,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
try:
 source=f'''/* Native x86-64 Linux control. RDI is deliberately a full 64-bit value. */
.global _start
.section .text
_start:
    movabs $0x100000001, %rdi
    lea payload(%rip), %rsi
    mov $3, %edx
    mov $0x63617077, %r10d
    mov $17, %r8d
    mov $0x9876, %r9d
    mov $1, %eax
    syscall
    mov %rax, raw_rax(%rip)
    /* Retain RAX unchanged before writing the independent binary report. */
    mov ${report_fd}, %edi
    lea record(%rip), %rsi
    mov $72, %edx
    mov $1, %eax
    syscall
    cmp $72, %rax
    jne failed
    cmpq $3, raw_rax(%rip)
    jne failed
    xor %edi, %edi
    mov $60, %eax
    syscall
failed:
    mov $1, %edi
    mov $60, %eax
    syscall
.section .rodata
payload:
    .ascii "abc"
.section .data
record:
    .quad 1
    .quad 0x100000001
    .quad payload
    .quad 3
    .quad 0x63617077
    .quad 17
    .quad 0x9876
    .quad 64
raw_rax:
    .quad 0
.section .note.GNU-stack,"",@progbits
'''
 with (D/'native-write.S').open('x') as f:f.write(source)
 phases=[]
 phases.append(run('assemble',[str(assembler),'--64','-o',str(D/'native-write.o'),str(D/'native-write.S')],2,5,256*1024*1024,64*1024))
 phases.append(run('link',[str(linker),'--build-id=sha1','-o',str(D/'native-write.elf'),str(D/'native-write.o')],2,5,256*1024*1024,64*1024))
 with (D/'native-disassembly.stdout').open('xb') as f:
  p=subprocess.run([str(objdump),'-d',str(D/'native-write.elf')],stdout=f,stderr=subprocess.PIPE,timeout=5)
 exclusive_json('DISASSEMBLY.json',dict(argv=p.args,returncode=p.returncode,stdout=rec(D/'native-disassembly.stdout'),stderr=p.stderr.decode()))
 assert p.returncode==0
 elf_before=rec(D/'native-write.elf')
 phases.append(run('native-write',[str(D/'native-write.elf')],1,3,16*1024*1024,4096,pass_fds=(report_fd,)))
finally:os.close(report_fd)
raw=raw_path.read_bytes();assert len(raw)==72
words=struct.unpack('<8Qq',raw)
result=dict(syscall_number=words[0],raw_arguments=list(words[1:7]),raw_fd_hex=hex(words[1]),argument_width_bits=words[7],raw_signed_rax=words[8],stdout=(D/'native-write.stdout').read_bytes().hex(),stderr=(D/'native-write.stderr').read_bytes().hex(),native_exit_status=phases[-1]['raw_status'],exact_three_bytes_written=(D/'native-write.stdout').read_bytes()==b'abc',full_64_bit_rdi_in_disassembly='movabs $0x100000001,%rdi' in (D/'native-disassembly.stdout').read_text(),elf_before=elf_before,elf_after=rec(D/'native-write.elf'),record=rec(raw_path),source=rec(D/'native-write.S'),report_fd=report_fd,phases=phases)
exclusive_json('NATIVE-RESULT.json',result)
assert result['raw_signed_rax']==3 and result['exact_three_bytes_written'] and not result['stderr'] and result['full_64_bit_rdi_in_disassembly'] and result['elf_before']==result['elf_after'],result
print(json.dumps({k:v for k,v in result.items()if k!='phases'},indent=2))
