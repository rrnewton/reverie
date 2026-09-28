from pathlib import Path
import hashlib,json,difflib
D=Path(__file__).resolve().parent;P=D.parent/'positive-v2';Q=D.parent/'positive-v2-continuation'
# Continue only checks using the actually qualified original compile and ELF records.
p=Q/'prepare.py';s=p.read_text();s=s.replace("name=sys.argv[1];kind=", "name=sys.argv[1];require(name in ('format','clippy'),'continuation only');kind=")
s=s.replace("HERE/'controls/compile/", "Path("+repr(str(P))+ ")/'controls/compile/")
p.write_text(s)
p=Q/'cache_lease.py';before=p.read_text()
needle="                final = observed.get('final_accounting') or observed.get('refusal_final_accounting')\n                require(final and final['cgroup_empty'] and final['properties']['MainPID'] == 0 and\n                        final['properties']['ControlGroup'] == '' and\n                        final['properties']['ActiveState'] in ('inactive','failed'), 'previous service not terminal')"
replacement="""                if result.get('kind') == 'startup-refusal-lifecycle-only':
                    # Exact owned startup refusal: no CPU-accounting fiction, no pass.
                    require(completion['result']['sha256'] == 'd240c378035634468803f123d816b5da9937837892709efbb57a35d4c95b84f9' and
                            prior['plan_sha256'] == 'bef6ca891e07f277a85926cf736648a6f041731f9514c00c367af14f7222e0ff',
                            'unbound lifecycle recovery')
                    require(result['accepted'] is False and result['observer_attempted'] is True and
                            result['accounting_complete'] is False and result['comparison_eligible'] is False and
                            result['payload_authorized'] is False and result['source_inputs_unchanged'] is True,
                            'lifecycle recovery scope changed')
                    for record in [result['original_result'], result['plan'], result['safehermit_report'], *result['observer_sources']]:
                        check_file(record)
                    require(all(not Path(p).exists() for p in result['absent_payload_records']), 'unexpected payload record')
                    require(len(result['fresh_checks']) == 2, 'missing quiescence checks')
                    for check in result['fresh_checks']:
                        for key in ['stdout', 'stderr']: check_file(check['query'][key])
                        props = check['properties']
                        require(props['Id'] == 'safehermit-20260918T083343Z-2735826.service' and
                                props['LoadState'] == 'not-found' and props['ActiveState'] == 'inactive' and
                                props['SubState'] == 'dead' and props['ControlGroup'] == '' and props['MainPID'] == '0' and
                                all(p['current'] is None for p in check['retired_processes']), 'incomplete lifecycle proof')
                else:
                    final = observed.get('final_accounting') or observed.get('refusal_final_accounting')
                    require(final and final['cgroup_empty'] and final['properties']['MainPID'] == 0 and
                            final['properties']['ControlGroup'] == '' and
                            final['properties']['ActiveState'] in ('inactive','failed'), 'previous service not terminal')"""
assert needle in before
p.write_text(before.replace(needle,replacement))
patch=''.join(difflib.unified_diff(before.splitlines(True),p.read_text().splitlines(True),fromfile='positive-v2/cache_lease.py',tofile='positive-v2-continuation/cache_lease.py'))
(D/'CONTINUATION-LEASE.patch').write_text(patch)
for name in ['prepare.py','cache_lease.py']:
 print(name,hashlib.sha256((Q/name).read_bytes()).hexdigest())
