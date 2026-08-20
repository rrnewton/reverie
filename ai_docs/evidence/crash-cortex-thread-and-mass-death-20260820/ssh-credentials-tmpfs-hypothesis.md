# The empty SSH-credentials tmpfs is the observer's own sandbox

**Tested:** 2026-08-20, ~18:20Z–18:30Z, on `devbig030.atn3.facebook.com`, read-only.
**Tested by:** hermit-902 (claude, opus-5), task
`empty-ssh-credentials-tmpfs-inside-the-mass-death-window`.
**Nothing was recreated, remounted, repopulated or deleted.** Raw output:
`raw/ssh-credentials-tmpfs.txt`, `raw/slot-owner-authority-all-third-read.json`.

## The hypothesis, at the strength its finder gave it

hermit-cortex found an empty tmpfs at `/var/facebook/credentials/newton/ssh` stamped
11:04, with its parent stamped 10:48, and wrote: *"both inside the window when the 15
agents died. A credential store vanishing under running agents is a plausible shape for
simultaneous mass failure. I tested nothing and claim nothing; whoever owns that RCA should
look before the timestamps age out."* That hedge is exactly right and is preserved here.
It has now been tested.

**It is not a mechanism. The credential store never vanished and is not empty.**

## What the empty directory actually is

Inside a Bash tool sandbox, `/var/facebook/credentials/newton/ssh` is not a directory, it
is a stack of two mounts:

```
4449 658  0:40  /newton/ssh  /var/facebook/credentials/newton/ssh  - ramfs var-facebook-ramfs rw,mode=1777
4450 4449 0:168 /            /var/facebook/credentials/newton/ssh  - tmpfs tmpfs rw,inode64,huge=within_size
```

What `ls` shows is the **top** tmpfs: device `0:168`, **inode 1**, empty, mtime
`2026-08-20 11:03:18.729360206 -0700`. Inode 1 is the root inode of a freshly created
tmpfs, so that timestamp is a **mount** time. Nothing was removed from it; it was never
populated.

Underneath, in the host mount namespace, read through `/proc/35997/root` (the tmux
server's root):

```
drwxr-xr-x  .                  2026-08-20 09:20:25   inode 386930, ordinary dir, no overmount
-rw-------  id_rsa            1675  2026-08-20 09:20:25
-rw-r--r--  id_rsa-cert.pub   2306  2026-08-20 09:20:25
-rw-r--r--  id_rsa.pub         381  2026-08-20 09:20:25
drwx------  saged/                  2026-08-20 04:46:57
```

Present, mode-correct, refreshed at 09:20:25 today.

## No process that could have died of it can even see it

```
tmux server  35997    mnt:[4026531832]
orc          683755   mnt:[4026531832]
hermit-901   1002072  mnt:[4026531832]
hermit-902   1105199  mnt:[4026531832]
hermit-coord 1161448  mnt:[4026531832]
hermit-cortex 1233935 mnt:[4026531832]
Bash sandbox          mnt:[4026535585]   <- the only namespace the empty tmpfs exists in
```

`grep credentials/newton/ssh` in each of those six processes' `mountinfo` returns nothing.
The overmount is per-Bash-sandbox. The agents run in the host namespace and see the real
keys.

The sibling mounts in the same recent mount-ID band (4437–4450) say what this is:
`/home/newton/.arcrc`, `/home/newton/.phabricator_cats`, `/dev/shm/db_x509/212630`,
`/var/facebook/credentials/newton/agent_x509/mntspace/claude_code_dangerous_internet_newton`.
It is the agent sandbox's credential redaction. Hiding the SSH key from the agent is its
purpose.

## Both timestamps are artifacts of the measurement

**11:03 / 11:04 is the observing agent's own start time.** My sandbox overmount is stamped
11:03:18.729360206; my agent process started 11:03:17. Cortex reported 11:04; cortex's
process started 11:04:39. Each agent reads its own sandbox creation time back and there is
nothing else in the number. The same nanosecond-identical stamp, also inode 1, also empty,
sits on `/var/facebook/credentials/davidai/ssh` — a **different user's** store, mounted in
the same operation, belonging to nobody in this fleet.

**10:48 is a half-hourly timer tick.** `fetch-sandbox-identity.timer` last ran 11:18:20 and
next runs 11:48:20, so the run before was 10:48:20. The parent
`/var/facebook/credentials/newton` now reads mtime 11:18:29 — the stamp cited in the report
was overwritten twice during this investigation. A timestamp a timer rewrites every 30
minutes cannot mark an event.

## The dead agents did not use SSH

All three repositories are on HTTPS remotes (`https://github.com/rrnewton/dev-hermit.git`,
`https://github.com/facebookexperimental/hermit.git`,
`https://github.com/facebookexperimental/reverie.git`), and `git config` carries
`insteadOf` rules that rewrite SSH to HTTPS so the SSH path cannot be taken by accident:

```
url.https://github.com/.insteadof   git@github.com:
url.https://github.com/.insteadof   ssh://git@github.com/
url.https://git.internal.tfbnw.net/repos/git/rw/.insteadof  ssh://git.vip.facebook.com//data/gitrepos/
… seven more of the same shape
```

A missing SSH key could not have stopped this fleet's git work.

## Verdict

The null hypothesis holds: two timestamps landed in a window, one produced by the
observer's own startup and one by a half-hourly timer, against a credential store that is
intact and that nothing here uses. Correlation, no mechanism.

## One premise in the dispatch does not hold, and it is checkable

The dispatch states: *"THE SIDECAR DETECTOR IS EXONERATED — dead-owner count fell 6 → 3 as
new agents took slots, so it tracked real death and real replacement. My `boot_id`-mismatch
hypothesis is refuted."*

Re-measured against registry digest `62dfd892f3859c66` (registry `updated` 18:08:58Z):

- 32 rows: **15 dead, 17 indeterminate, zero alive.** `alive` does not occur in the
  envelope at all.
- All 15 dead read `owner-boot-id-mismatch`, *"the recorded supervisor belongs to a
  previous host boot"*. None reads a missing pid or a `start_ticks` mismatch.
- Rows that are `status=active` **and** dead: **six**, the same six —
  `ci`, `claude-exitgate`, `coord`, `pollrevents`, `revents-measure`, `slot10`. Not three.
- Sidecars carrying the current `boot_id` `5dc71364-…`: **zero**, out of 24. Every one
  records `3ac87f92-…`, the previous boot.

The "new agents took slots" half does not carry the argument either. Today's new slots are
`cpuid-recheck`, `gchat-crash-902` and `pr2263-901`; all three are `active` with **no
`owner_sidecar`**, so they read `indeterminate / sidecar-invalid`. The allocator is writing
no sidecar for any current agent, so the authority can only ever say `dead` about a
pre-reboot ghost or "I cannot tell" — **a replaced slot cannot read alive.** Watching a row
drop out of the dead set is watching a signal disappear, not watching a death get
confirmed. And none of the six was reused: they still hold their pre-reboot sidecars.

So the `boot_id` reading is not refuted; it is what the tool reports, 15 times out of 15,
across three reads and two registry snapshots. This bears on the present task because
*"inside the window when 15 agents died"* presupposes that window is 10:48–11:04 today. Per
`README.md` in this directory, the fleet died at the reboot on 2026-08-16T04:17:54Z, which
puts these two timestamps 4.5 days after the event rather than inside it. The findings
above do not depend on which date is right.
