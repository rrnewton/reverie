import json,os,sys,re,shutil,tempfile,subprocess,signal
sys.path.insert(0,'/home/newton/work/dev-hermit/ignored/kvm2-q0120')
import qual
rows={r['cell']:r for r in json.load(open(qual.OUT+'/slice.json'))}
out=[r for r in json.load(open(qual.OUT+'/out.json')) if r['verdict']=='QUALIFIED']
ARG=re.compile(r'usage|unknown pattern|unrecognized|invalid option|missing argument',re.I)
res=[]
for r in out:
    row=rows[r['cell']]
    src=open(os.path.join(qual.REPO,row['fixture']),errors='replace').read()
    mut,desc=qual.mutate(src,row['family'])
    os.makedirs(qual.STAGE,exist_ok=True); wd=tempfile.mkdtemp(prefix='rc-',dir=qual.STAGE)
    try:
        m=os.path.join(wd,'mut')
        ok,_=qual.cc(mut,m,row.get('cflags',[]))
        if not ok: res.append({'cell':r['cell'],'note':'mutant no longer compiles'}); continue
        v=qual.hermit(m,wd)
        blob=v['out']+"\n"+v['err']
        # the guest's own streams are echoed by hermit under Stdout:/Stderr: headings
        gs=re.search(r'Stdout:\n(.*?)\n\s*\nStderr:\n(.*?)\n\s*\n',blob,re.S)
        gout=(gs.group(1).strip() if gs else v['out'].strip())
        gerr=(gs.group(2).strip() if gs else '')
        res.append({'cell':r['cell'],'rc':v['rc'],'guest_stdout':gout[:70],'guest_stderr':gerr[:70],
                    'arg_error':bool(ARG.search(gout) or ARG.search(gerr))})
    finally: shutil.rmtree(wd,ignore_errors=True)
    print("%-30s rc=%s arg_err=%s | out=%r | err=%r"%(r['cell'].split('/')[-1],res[-1].get('rc'),
          res[-1].get('arg_error'),res[-1].get('guest_stdout'),res[-1].get('guest_stderr')),flush=True)
json.dump(res,open(qual.OUT+'/recheck.json','w'),indent=1)
