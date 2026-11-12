from pathlib import Path
import hashlib,json
D=Path(__file__).resolve().parent;slot=D.parents[1];old=slot/'ignored/prejoin-failure-implementation-20260917/queue-refresh-20260917-v2';q=json.loads((D/'QUEUE-DELTA.json').read_text());h=lambda b:hashlib.sha256(b).hexdigest()
checks=[];raw=[]
for p in sorted(D.glob('*.receipt.json')):
 r=json.loads(p.read_text());name=p.name.removesuffix('.receipt.json');assert r['exit_code']==0 and not r['timed_out'];assert r['argv'][:5]==['/usr/bin/with-proxy','gh','api','--method','GET'];assert r['bounds']=={'cpu_seconds_per_process':15,'wall_seconds':60,'address_space_limit':'inherited; no added RLIMIT_AS','bytes_per_stream':16777216,'core_bytes':0}
 for chan in ['stdout','stderr']:
  data=(D/(name+'.'+chan)).read_bytes();assert len(data)==r[chan+'_bytes'] and h(data)==r[chan+'_sha256'] and len(data)<16777216
 checks.append({'operation':name,'result_hash_and_bound_checks':True,'exit_code':r['exit_code'],'wall_seconds':r['wall_seconds'],'ended_at':r['ended_at']});raw.append(r)
for repo in ['hermit','reverie']:
 xs=json.loads((D/(repo+'-open.stdout')).read_text());assert len(xs)==q[repo]['count'] and len(xs)<100 and all(x['state']=='open' for x in xs)
 ref=json.loads((D/(repo+'-main.stdout')).read_text());assert ref['ref']=='refs/heads/main' and ref['object']['type']=='commit' and ref['object']['sha']==q[repo]['remote_main']
checks_by_pr=[]
for n in [3066,3069]:
 p=json.loads((D/f'hermit-{n}.stdout').read_text());fs=json.loads((D/f'hermit-{n}-files.stdout').read_text());snap=next(x for x in q['hermit']['open'] if x['number']==n);assert p['head']['sha']==snap['head_sha'];assert len(fs)==p['changed_files'] and len(fs)<100
 checks_by_pr.append({'url':p['html_url'],'head_sha':p['head']['sha'],'changed_files':len(fs),'files':[x['filename'] for x in fs],'scope':'shared validation classification' if n==3066 else 'shared validation cell-verdict provenance; relevant to KVM scorecards','source_review_performed':False})
assert q['reverie_578']['merged'] and q['reverie_578']['state']=='closed';assert [p['number'] for p in q['reverie']['open']]==[467]
report='''At 2026-09-17 20:24:47 UTC, the complete queues still contain 19 open Hermit pull requests and one open Reverie pull request. Relative to the retained 19:32 UTC v2 snapshot, none entered and none departed. Two Hermit entries changed head and returned base SHA; all other head/head-ref/base-ref/base-SHA/title/draft/state fields compared here are unchanged. Follow-up file and state reads finished at 20:25:36 UTC. This is queue and scope evidence, not source approval, ownership, test evidence or permission to land.

The separate explicit GETs of refs/heads/main returned:

| Repository | Current remote main |
| --- | --- |
| https://github.com/rrnewton/hermit | `c8c33db1e5b59aab9f7ead3c1c710bef0b68e2b6` |
| https://github.com/rrnewton/reverie | `7d863ab3f02639731713a01467b2548c41e3dbfb` |

These are direct remote-ref observations. The base objects in pull responses below are recorded separately and are not substitutes for a main-tip query. The 19:32 queue snapshot did not independently query main, so this report does not derive a main-tip change from its old PR base objects.

The complete open population is:

| Full URL and title | Exact head | State | Returned base |
| --- | --- | --- | --- |
'''
for repo in ['hermit','reverie']:
 for p in q[repo]['open']:
  report+=f"| {p['url']} — {p['title']} | `{p['head_sha']}` | Open{' draft' if p['draft'] else ', non-draft'} | `{p['base_ref']}` / `{p['base_sha']}` |\n"
report+='''
The two changes are confined to already known shared-validation proposals:

- https://github.com/rrnewton/hermit/pull/3066 advanced from `b27530786ae57fb9cc43d9d49563ce8f02e64013` to `6752f558fdc6049326b763bdb63603762b54aa77`, with returned main base `c8c33db1e5b59aab9f7ead3c1c710bef0b68e2b6`. Its complete current inventory is one file, `scripts/lib/validate_classification.rs`. Its unchanged body describes distinguishing unwritten structured test results from measured product failures. This affects interpretation of KVM validation results along with every other backend; it introduces no KVM runtime path in the current inventory. Its correctness and claimed controls were not reviewed here.
- https://github.com/rrnewton/hermit/pull/3069 advanced from `9cec06dcc9d8acf4828b72f73d9bb1abd1f01ddc` to `858a65e93d5ffe289bd6a329a67d62d4ecc42989`, with the same returned main base. Its body and all four returned per-file patch texts match the retained earlier query. Current files remain `ci/manifest-plan/src/ledger.rs`, `ci/manifest-plan/src/ledger/schema10.rs`, `ci/manifest-plan/src/ledger/schema10/tests.rs`, and `scripts/lib/validate_cell_results.rs`. Its scope remains binding compared cell verdicts to their selected attempt, relevant to KVM scorecard provenance and separate from backend implementation. Equality of the returned diff text does not assert complete head-tree identity or carry an old source approval to the new head.

For both, post-file-query PR metadata still named the initial snapshot head and changed_files equalled the complete returned inventory. Their titles, branches and draft state are unchanged. No new KVM-related proposal or new runtime scope was found. There was therefore no reason to re-download or re-review the unchanged donor diffs.

The established dispositions remain distinct:

- https://github.com/rrnewton/hermit/pull/2958 and https://github.com/rrnewton/hermit/pull/2969 remain the broad harness-managed verification donor and successor at their exact prior heads. Preserve the root's per-obligation accounting and same linear continuation; independently landed increments do not by themselves discharge the whole donors.
- https://github.com/rrnewton/hermit/pull/2694 remains the inherited-stdio/append work with its separately established startup-failure and open-file-description lifetime obligations. The later pre-join cleanup landing is not a new proof of those separate contracts.
- https://github.com/rrnewton/hermit/pull/2302 remains the M2 strict-default/lossy-removal obligation. The newly prepared M2 v5 preview is isolated, uncompiled and unlanded; it has not changed this public head or closed the obligation. Preserve numeric/INFO-divergence controls in the same continuation.
- https://github.com/rrnewton/hermit/pull/2747 remains the established SaBRe-only physical-exit work, distinct from KVM's pre-join failure repair. The separate SaBRe application https://github.com/rrnewton/hermit/pull/2836 is likewise unchanged.
- https://github.com/rrnewton/reverie/pull/467 remains the foreign DBT admission draft at `51272804a0073c60d3dc73dbd396ba664f934bf9`. Being the only open Reverie PR does not transfer it to this lane. The Hermit DBT host-logging proposal https://github.com/rrnewton/hermit/pull/1689 also remains distinct.

https://github.com/rrnewton/reverie/pull/578 was queried explicitly: state closed, merged true, PR head `12d4ce8c0bc426f1ae41416f5b4a699e2c300879`, merge commit `7d863ab3f02639731713a01467b2548c41e3dbfb`, merged at 2026-09-17T18:48:15Z. This agrees with the separately queried current Reverie main. The complete open list contains only the known foreign draft, so no own Reverie PR is left open at this observation. This public-state read does not replace the previously retained landing content proof.

All nine network operations were explicit GETs wholly wrapped in `/usr/bin/with-proxy`, with the established 15 CPU seconds per process, 60 wall seconds and 16 MiB per output-stream bounds, zero core files and normal inherited address space. All succeeded; stderr was empty. Each complete open list returned fewer than the requested 100 entries, so no second page was needed. The three earlier local runtime/cgo aborts under an added 1 GiB address-space cap remain untouched in the old v1 directory and were not repeated or used as remote evidence. No claim, PR comment, label, review, closure, source, index, ref, cache, package or runtime change was made. No build, test, VM or guest ran.

`QUEUE-DELTA.json` retains every current full URL, exact head, returned base and selected metadata change. `PREPARATION.json` binds the retained 19:32 snapshot, prior reports, reader and proxy skill. The raw responses and exact command receipts are adjacent. `READBACK.json` authenticates the retained outputs and scope inventories; `MANIFEST.json` binds this finished report and those artifacts. Root retains live TaskGraph authority and landing decisions.
'''
(D/'REPORT.md').write_text(report)
(D/'READBACK.json').write_text(json.dumps({'operations':checks,'current_scope':checks_by_pr,'queue_counts':{'hermit':q['hermit']['count'],'reverie':q['reverie']['count']},'complete_queues':True,'open_own_reverie_prs':0,'reverie_578_merged':True,'scope':'read-only queue/input authentication; no source review or product execution'},indent=2)+'\n')
manifest=[]
for p in sorted(D.iterdir()):
 if p.is_file() and p.name!='MANIFEST.json':
  b=p.read_bytes();manifest.append({'path':str(p),'bytes':len(b),'sha256':h(b)})
(D/'MANIFEST.json').write_text(json.dumps({'files':manifest},indent=2)+'\n')
for p in [D/'REPORT.md',D/'QUEUE-DELTA.json',D/'READBACK.json',D/'MANIFEST.json']:
 print(p.name,len(p.read_bytes()),h(p.read_bytes()))
