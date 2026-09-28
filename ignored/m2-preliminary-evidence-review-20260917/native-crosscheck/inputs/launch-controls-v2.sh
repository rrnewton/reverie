#!/usr/bin/bash
set -u
cd /home/newton/work/dev-hermit/worktrees/slots/kvm-prejoin-main-20260917
packet=ignored/m2-controls-v1
for phase in native-m2-detcore native-m2-hermit-reports native-m2-hermit-comparators; do
    /usr/bin/python3 -B "$packet/prepare-control-v2.py" "$phase" > "$packet/$phase-prepare.stdout" 2> "$packet/$phase-prepare.stderr"
    m2_prepare_status=$?
    printf '%s\n' "$m2_prepare_status" > "$packet/$phase-prepare.status"
    if [ "$m2_prepare_status" -ne 0 ]; then exit "$m2_prepare_status"; fi
    m2_plan_sha=$(/usr/bin/python3 -B -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1],"rb").read()).hexdigest())' "$packet/$phase-plan.json")
    m2_hash_status=$?
    if [ "$m2_hash_status" -ne 0 ]; then exit "$m2_hash_status"; fi
    /usr/bin/python3 -B "$packet/run_phase.py" \
        "/home/newton/work/dev-hermit/worktrees/slots/kvm-prejoin-main-20260917/$packet/$phase-plan.json" \
        "$m2_plan_sha" > "$packet/$phase-launch.stdout" 2> "$packet/$phase-launch.stderr"
    m2_control_status=$?
    printf '%s\n' "$m2_control_status" > "$packet/$phase-launch.status"
    if [ "$m2_control_status" -ne 0 ]; then exit "$m2_control_status"; fi
done
