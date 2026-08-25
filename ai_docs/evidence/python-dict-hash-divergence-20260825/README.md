# `language-runtimes/python-dict-hash-iteration` — first-divergence diagnosis

Measured 2026-08-25 by `agent(hermit-006)` against the owner's failing validate
`validate-full-4e168f2aa5b9-20260825T201958Z`, hermit `4e168f2aa5b9`,
binary sha256 `027058ffa844e3ff98045837f549861e7a13cf6bae99f6bffa6c11c5d8e9321f`.
Node `e2e.manifest_language_runtimes`; coordinate turn 530, vns 1767225606089924540,
record 22740, syscall 2338.

## Verdict

**Neither hash randomisation nor a scheduling difference. A live host file.**
Classification for the green gate: **BAR RAISE**, not a regression.

## The first divergence

    Mismatch at log messages 22740 / 22740:
      < DETLOG [iobuf][dtid 30] read in fd=35 0x7ffff5d8dc00+246->9e6af3106cb03243...
      > DETLOG [iobuf][dtid 30] read in fd=35 0x7ffff5d8dc00+246->127eac492aec3a66...
    Identical in both runs:
      #2336 openat(-100, "/var/facebook/configerator-client-system-configs/
                          falcon/core_entities/0/23435333333313533373", O_CLOEXEC) = Ok(35)
      #2337 newfstatat(35, "", {S_IFREG|0644, st_size=246}, AT_EMPTY_PATH) = Ok(0)
      #2338 read(35, 0x7ffff5d8dc00, 247) = Ok(246)

Same fd, address, length and return value. Only the content differs.

## Hash randomisation: refuted, with the negation demonstrated

The first mismatch is record 22740 of 34973. Records 1..22739 matched bitwise, and that
span contains the entire CPython startup, every `hash()`, the set/frozenset/dict
construction, the `seed.txt` write and its sha256, and the `/usr/bin/sort` subprocess —
all direct functions of the SipHash seed, all printed or written. A differing seed cannot
leave them identical.

Control, so the match is a positive result rather than a vacuous one — two NATIVE runs:

    python3 -c 'print(hash("alpha"))'   ->  -1527078356523335011
                                        ->  -1520671509866288995

Randomisation is live on this host; under hermit 22739 consecutive records containing all
of it matched. **Hermit's entropy determinization is working.**

## Scheduling: refuted

`34974|34974` total, `34973|34973` INFO, `33699|33699` DETLOG+COMMIT, identical syscall
numbering, `clock_gettime` returning `tv_nsec 87887990` and `87898950` identically on both
sides, exit 0 both.

## The source, measured live

`/var/facebook/configerator-client-system-configs/falcon/core_entities/0/23435333333313533373`
— 246 bytes, owner `falcon`, **rewritten with different content every ~15–20 s**.
Two consecutive versions differ in **19 of 246 bytes**; the payload decodes to
`configerator … /falcon/ensembleMonitoring/heartbeat`, so the moving field is a monitoring
heartbeat timestamp. Constant length is why the syscall stream cannot see it and only the
io-buffer hash can.

**Of 56 entities in that directory, exactly one mutates** — this one — cross-checked with
`stat` and with `find -newermt "@<epoch>"`, which agree.

## The contact is compiled into the interpreter

`python3` on devbig014 is `/usr/local/fbcode/platform010/bin/fbpython`, Python 3.12.14+meta.

    strace -f -e trace=openat python3            -c pass   6 hits under /var/facebook
    strace -f -e trace=openat python3 -S -I      -c pass   6 hits
    strace -f -e trace=openat env -i <fbpython> -S -I -c pass   6 hits
    /bin/true                                              0 hits
    /usr/bin/sort                                          0 hits

Not reachable from the test's flags or environment. `--base-env=minimal` cannot help: it
scrubs the environment, and this is a filesystem read.

## Nothing is behind it

The run's own retained logs (`/tmp/run1_log_6mIt7`, `/tmp/run2_log_OzvHj`, 5788690 bytes
each) were diffed after replicating the two declared transforms — strip
`real-wall-clock-prefix/v1`, ordinalize host addresses:

    DIFFERING LINES: 1 of 36341        by kind: {'[iobuf]': 1}

Zero `[syscall]`, zero `COMMIT`, zero other. A raw diff shows 34974 of 36341 lines
differing purely because every line carries a wall-clock prefix the comparator strips;
that number is an artefact.

## Exposure

**Two cells, not five.** Exposure follows the interpreter, not the backend:

| cell | interpreter | /var/facebook hits | ms | outcome |
| --- | --- | --- | --- | --- |
| `python-dict-hash-iteration` | fbpython | 6 | 22138 | FAIL |
| `python-io-subprocess-time` | fbpython | 6 | 7893 | PASS (quiet) |
| `python-hash-determinism` | `/usr/bin/python3` 3.9 | 0 | 3384 | PASS |
| `python-random` | `/usr/bin/python3` 3.9 | 0 | 2774 | PASS |
| `applications/kvm-python-examples` | `/usr/bin/python3` 3.9 | 0 | 56513 | PASS |

`python-io-subprocess-time` is exposed and passing on a short window, not safe.

## Why bar raise

Both runs read 246 bytes and both return `Ok(246)`. The pre-`#2384` comparator saw only the
return value and passed it; `DETLOG [iobuf]` records did not exist before `8bc26dba1d53`, so
this cell **could not have failed for this reason at any earlier commit**. Structural
attribution, not statistical. Precedent: hermit#2449 / `afee8dcd8d9a` made the same call on
the sibling cell (*"THE COUNT GOES DOWN AND BECOMES TRUE … 279 → 278"*).

**No green to retract** — this cell was already recorded red, so the published count does
not move.

## Limits

- The validate's own binary was cleaned, so the divergence was read from the retained
  artifact rather than re-executed.
- The normalisation used for the full-log diff approximates hermit's; the one surviving line
  carries hashes byte-identical to those hermit's comparator reported at its index 22740.
- One verify pair. It cannot exclude a divergence this pair never exercised.
- hermit#2449 recorded this same cell first-diverging at record 378 on a `newfstatat`
  directory-metadata difference. That is absent here, which is why it reads as fixed — but
  the commit that fixed it was not identified.

## Remedy

Not in this document. Isolating `/var/facebook` is fleet-wide and the platform emits
*"System-entities … are inaccessible in this environment. This is a dangerous mode of
operation."* Tracked as `owner_decision_isolate_var` with three shapes: mask for all e2e
guests, mask for the portable lane only, or add a contact detector.
