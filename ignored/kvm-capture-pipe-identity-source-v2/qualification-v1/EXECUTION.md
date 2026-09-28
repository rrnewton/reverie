# Authorized only after root inspects this frozen caller

Run once, without a pipeline, from the qualification directory:

```sh
PYTHONOPTIMIZE=0 /usr/bin/python3 -B execute.py run
```

`execute.py` checks all held caller/source inputs and absent destinations first. It then invokes private admission; for each of the 72 planned phases it invokes `prepare.py <name>` and `phase.py launch <exact-plan-path> <actual-SHA256>`. After accepted metadata it runs bind_dependencies.py; after accepted compile it runs retain_artifacts.py. It does not retry any step. A generated plan is accepted only when its exact phase receipt is accepted, raw zero, source unchanged and terminal authenticated. New raw errors remain failures even if a later independently authorized successor is prepared.

Concrete phase order and all prospective argv are in PHASE-PLAN.json. Artifact paths there are the exact intended retained destinations; they are not claims that ELFs already exist. Before execution those paths, target, private lease and phase directories must be absent. The exact actual Cargo-emitted originals and immutable copies are bound before any listing/test.

No other script, Cargo command, test selector, guest or source change is authorized by this caller. Source changes or a genuine first failure require a separately preserved successor and root disposition. The ongoing unrelated H39ac cancellation work uses its own source/target/lease and receives no credit from these tests.
