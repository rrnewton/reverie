from pathlib import Path
import datetime,json,subprocess,time
D=Path(__file__).parent
p=json.loads((D/"plan.json").read_text())
start=time.monotonic(); results=[]
for i,cmd in enumerate(p["commands"]):
 with (D/("build-"+str(i)+".log")).open("wb") as log:r=subprocess.run(cmd,stdout=log,stderr=subprocess.STDOUT)
 results.append({"phase":i,"native_exit":r.returncode})
 if r.returncode:break
result={"results":results,"wall_seconds":time.monotonic()-start,"finished_at":datetime.datetime.now(datetime.timezone.utc).isoformat()}
(D/"build-result.json").write_text(json.dumps(result,indent=2,sort_keys=True)+"\n")
print(json.dumps(result),flush=True)
raise SystemExit(results[-1]["native_exit"])
