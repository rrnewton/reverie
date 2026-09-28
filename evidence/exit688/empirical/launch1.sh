#!/bin/bash
D=/home/newton/work/dev-hermit/ignored/liteinst-lane-claude/exit688/empirical
echo "waiting for lock $(date -Is) loadavg=$(cat /proc/loadavg)" > $D/campaign1/lockwait.txt
flock /home/newton/work/dev-hermit/ignored/liteinst-lane-claude/flake/pressure.lock $D/campaign.sh $D/campaign1 200 5 ordinary_nonleader
echo "done rc=$? $(date -Is)" >> $D/campaign1/lockwait.txt
