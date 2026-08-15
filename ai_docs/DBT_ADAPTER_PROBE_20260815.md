# DBT adapter probe — does it publish an INFO comparison?

Measured 2026-08-15 by hermit-coord (devbig030). Live run against an already-enabled
dbt plan cell; nothing was enabled, qualified, or edited to obtain this.

## PROVENANCE — embedded so this copy can be told from a current one

```
probe binary        hermit 0.2.0 (2026-08-14, ge2f9a36b1ad8), target/release/hermit
origin/main at time 3aaaf2d0   <-- BINARY PREDATES MAIN; see the bound below
guest source        hermit/tests/c/kcmp_eperm.c  sha256 5d9f6ad1c844b96326f158bc378fb1bd9c6b01e62cabd89b52360f24b64fe836
guest binary        NOT COMMITTED (ELF). Reproduce: gcc -O0 -o kcmp tests/c/kcmp_eperm.c
source ptrace.json  sha256 6e00e84bee049985cc3cd107cd93aa1c23b8af9e65399dee33f70fa8e84168af
source dbt.json     sha256 aef37a7cccb959c03d465d2400cf9fca5b9058a333023f350a4ba84eb5b6369b
source d.err        sha256 53e00ecfc32d01294b9bf3ec34a4f35e707321b822370c8c699fc39f177fd851  (13455 bytes)
THE PROBE OUTPUT IS FINISHED, NOT LIVE. Promoted at a stopping point: the runs had
completed and the files had been quiet for two minutes when this snapshot was taken.
A one-shot copy of a MOVING file is a guarantee of future staleness; this one is not moving.
```

## COMMAND — identical but for `--backend`

```
env LC_ALL=C TZ=UTC ./target/release/hermit --log=info run --backend <B> \
    --strict --verify --verify-json <out>.json -- <workdir>/kcmp
```

Cell: `c-programs/kcmp-eperm`, already in `ci/expected-e2e-plan.json` for both backends.
Run outside `/tmp`: Hermit replaces guest `/tmp`, and a first attempt from there produced a
FALSE `no_result` on the ptrace CONTROL. That is why a control was run at all.

## RESULT

| field | ptrace (control) | dbt |
|---|---|---|
| `verdict` | `"matched"` | **`"no_result"`** |
| `verified` | `true` | **`false`** |
| `bitwise_parity` | `false` | **`false`** |
| `guest_exit_code` | `0` | **`null`** |
| `compared_log_messages` | `{"left": 88, "right": 88}` | **`null`** |
| `comparison` | populated (`strictness: stripped`) | **`null`** |
| process rc | 0 | 0 |

**The reason string holds: zero INFO messages compared, not a handful.**

## AND IT IS NOT SIMPLY BROKEN — THIS IS THE FINDING

The dbt run EXITED 0 and its own stderr reports a complete, verified execution:

```
    reverie-dbt: tool=Detcore branches=45196 syscalls=39 rewritten=38 stdin_reads=0 memory_hash=5d9478fc8d30b012
    :: Comparing DBT observed guest-memory hashes... 5d9478fc8d30b012 | 5d9478fc8d30b012
    :: DBT path confirmed: DynamoRIO client reported tool=Detcore
    :: Success: deterministic. Determinism verified.
```

DBT ran the real Detcore tool, executed 39 syscalls, compared two runs by guest-memory
hash and found them identical. **None of that reaches the verify JSON.** The adapter proves
determinism through a memory-hash channel and then publishes `no_result`, so a machine
consumer sees no comparison while a human reading the terminal sees "Determinism verified".

**A false-assurance defect with the opposite sign to the four found this session:** those
claimed more than they could support; this supports more than it claims. Same class,
inverted, and it is why dbt plan cells can be green in a run while contributing no L2 evidence.

## BOUND

n=1 is complete for a fixed property of an adapter, not a rate. **But the binary is
`ge2f9a36b1ad8` and `origin/main` is `61b9ba7aa`, so this measures the adapter as of that
build.** If the DBT adapter has been touched since, re-run before relying on it. No rebuild
was done: a long job for a property measurable now and labellable.
