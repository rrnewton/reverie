# Independent recount from the raw stderr files, without parse_runs.py.
# Usage: awk -f recount.awk /tmp/cml-attr749-1227/runs/*.err
# Prints, per variant: marker lines, markers with >=1 child in flight,
# markers with >=2 in flight, and the largest (fds - iteration-0 fds).
FNR == 1 {
    v = FILENAME; sub(/.*\//, "", v); v = substr(v, 1, 1)
    base = -1
}
/^capacity marker: iteration=/ {
    line = $0
    match(line, /iteration=[0-9]+/); it = substr(line, RSTART + 10, RLENGTH - 10) + 0
    match(line, /fds=[0-9]+/); fds = substr(line, RSTART + 4, RLENGTH - 4) + 0
    if (it == 0) base = fds
    if (line ~ /finished_after_hooks_range=/) {
        match(line, /finished_after_hooks_range=[0-9]+\.\.[0-9]+/)
        r = substr(line, RSTART, RLENGTH); sub(/.*\.\./, "", r); fin = r + 0
    } else {
        rest = line; sub(/.*finished_after_hooks=\[/, "", rest)
        fin = gsub(/\([0-9]+, (true|false), (true|false), (true|false)\)/, "", rest)
    }
    infl = it + 1 - fin
    markers[v]++
    if (infl >= 1) ge1[v]++
    if (infl >= 2) ge2[v]++
    ex = fds - base
    if (!(v in maxex) || ex > maxex[v]) maxex[v] = ex
}
END {
    for (v in markers)
        printf "%s markers=%d inflight_ge1=%d inflight_ge2=%d max_excess=+%d\n", v, markers[v], ge1[v] + 0, ge2[v] + 0, maxex[v]
}
