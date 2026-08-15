import json, sys
sys.path.insert(0,'/home/newton/work/dev-hermit/ignored/kvm2-screen')
from value_mutate import screen_value
rows=json.load(open('refusable86.json')); out=[]
for i,r in enumerate(rows):
    v=screen_value(r['program']); v['id']=r['id']; v['check_channel']=r['check_channel']; out.append(v)
    if i%15==0: print(i,'/',len(rows),flush=True)
json.dump(out,open('value_pass3.json','w'),indent=1)
from collections import Counter
print(Counter(x['verdict'] for x in out))
