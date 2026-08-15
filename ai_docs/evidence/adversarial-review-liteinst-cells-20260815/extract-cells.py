import tomllib, subprocess, sys, json
ref=sys.argv[1]
files=subprocess.run(['git','ls-tree','-r','--name-only',ref,'tests/e2e/manifests/'],
                     capture_output=True,text=True).stdout.split()
out={}
for f in files:
    if not f.endswith('.toml'): continue
    blob=subprocess.run(['git','show',f'{ref}:{f}'],capture_output=True).stdout
    d=tomllib.loads(blob.decode())
    for t in d.get('test',[]):
        for mode,spec in t.get('modes',{}).items():
            for be in spec.get('backends_enabled',[]):
                out[f"{t['id']}|{mode}|{be}"]={'ci':spec.get('ci'),
                    'assert':spec.get('assert')}
print(json.dumps(out))
