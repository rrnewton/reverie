#!/bin/bash
set -euo pipefail
artifact=/home/newton/work/dev-hermit/experiments/liteinst-xrstor-metadata-20260905
cd "$artifact"
attempt=attempt-02
mkdir "$attempt"
cp probe.c probe.S run.sh run.json "$attempt/"
exec > "$attempt/run.log" 2>&1
set -x
if env | cut -d= -f1 | grep -Eq '^(LD_|GLIBC_TUNABLES$)'; then exit 78; fi
if test -e /etc/ld.so.preload || test -L /etc/ld.so.preload; then exit 78; fi
ulimit -c 0
date --iso-8601=seconds
uname -a
lscpu
cc --version
sha256sum /usr/bin/cc /lib64/libc.so.6 /lib64/ld-linux-x86-64.so.2 /etc/ld.so.cache > "$attempt/platform.sha256"
sha256sum probe.c probe.S run.sh run.json > "$attempt/source.sha256"
set +e
cc -std=c11 -O2 -Wall -Wextra -Werror -mgeneral-regs-only -fno-tree-vectorize probe.c probe.S -o "$attempt/probe"
compile_status=$?
set -e
printf '%s\n' "$compile_status" > "$attempt/compile.status"
if test "$compile_status" -ne 0; then exit "$compile_status"; fi
sha256sum "$attempt/probe" > "$attempt/binary.sha256"
objdump -d -Mintel "$attempt/probe" > "$attempt/disassembly.txt"
readelf -W -h -l -d "$attempt/probe" > "$attempt/elf.txt"
failed=0
for profile in zero nonzero; do
    for mode in capture restore; do
        directory="$attempt/$profile-$mode"
        mkdir "$directory"
        printf '%s\n' "$artifact/$attempt/probe $mode $profile $artifact/$directory" > "$directory/command"
        set +e
        "$artifact/$attempt/probe" "$mode" "$profile" "$artifact/$directory" > "$directory/stdout" 2> "$directory/stderr"
        result=$?
        set -e
        printf '%s\n' "$result" > "$directory/status"
        if test "$result" -ne 0; then failed=1; fi
    done
done
sha256sum -c "$attempt/source.sha256"
sha256sum -c "$attempt/binary.sha256"
sha256sum -c "$attempt/platform.sha256"
printf '%s\n' "$failed" > "$attempt/run.status"
find "$attempt" -type f ! -name evidence.sha256 ! -name run.log -print0 | sort -z | xargs -0 sha256sum > "$attempt/evidence.sha256"
exit "$failed"
