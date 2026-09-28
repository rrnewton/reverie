from pathlib import Path
import subprocess,json,time,datetime,os
E=Path(__file__).parent
R=E.parents[2]
# This evidence directory is nested beneath RS/ignored/task.
R=Path("/home/newton/work/dev-hermit/worktrees/slots/ci-signal-main-health-20260919/reverie")
env=os.environ.copy();env["CARGO_TARGET_DIR"]="/home/newton/work/dev-hermit/worktrees/slots/pr-drain-ci-signal-parent-scorecard-20260920/hermit/target";env["CARGO_BUILD_JOBS"]="2"
commands=[["cargo","fmt","--all","--","--check"],["cargo","test","--offline","-p","reverie-sabre","--test","loader_plugin_finalizers","--test","late_function_registration","--","--nocapture"],["cargo","clippy","--offline","-p","reverie-sabre","--tests","--","-D","warnings"]]
(E/"commands.json").write_text(json.dumps(commands,indent=2)+"\n")
results=[];start=time.monotonic()
for i,cmd in enumerate(commands):
 t=time.monotonic()
 with (E/(str(i)+".log")).open("wb") as log:rr=subprocess.run(cmd,cwd=R,env=env,stdout=log,stderr=subprocess.STDOUT)
 result=dict(phase=i,argv=cmd,native_exit=rr.returncode,wall_seconds=time.monotonic()-t);results.append(result);(E/"results.json").write_text(json.dumps(results,indent=2)+"\n");print(json.dumps(result),flush=True)
 if rr.returncode:break
(E/"terminal.json").write_text(json.dumps(dict(results=results,wall_seconds=time.monotonic()-start,finished_at=datetime.datetime.now(datetime.timezone.utc).isoformat()),indent=2)+"\n")
raise SystemExit(results[-1]["native_exit"])
