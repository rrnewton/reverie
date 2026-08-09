# Hermit unit and integration test-driver inventory.
#
# Sources: hermit-cli/tests/*.rs, validate.sh, ci/dag/hosted.json,
# and ci/dag/hardware.json at base 6cd2b1d4716d165fed5c46bbeadeceebde7c9754.
# This file contains Cargo tests, validate.sh entrypoints, record/replay
# commands, Python drivers, and compound CI commands. It is intentionally
# non-executable; it is an audit/runbook, not a suite.
#
# Annotations:
#   [verify]       hermit run --verify (normally with --strict)
#   [record/replay] hermit record/replay, often record start --verify
#   [both]         the same validate.sh label is covered by both paths
#   [both: ...]    command-specific verify/record/replay arm of that label
#   [run]          repeated Hermit runs without built-in verification
#   [both/mixed]   a Cargo/CI driver contains more than one of those modes
#
# Runtime-created paths use descriptive shell variables such as $HOME_DIR,
# $RECORDING_DIR, $CARGO_TARGET_TMPDIR, and $HERMIT_LEVELDB_BUILD_DIR.
# validate.sh functional probes deliberately diverge: strict verification runs
# REAL_COMPAT_WORKLOAD, while selected R/R rows record the listed command.
# At this source snapshot, the rr control flow reaches 142/144 selected labels:
# tcl and dc are selected in RR_COMPAT_PASSING_LABELS but guarded to strict/SaBRe.
# System utilities
hermit record start --data-dir "$RECORDING_DIR" -- /bin/echo hermit-compat # [both: record] validate.sh:2462 label=echo
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2462 label=echo
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/true # [both: record] validate.sh:2464 label=true
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2464 label=true
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/pwd # [both: record] validate.sh:2466 label=pwd
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2466 label=pwd
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/seq 10 # [both: record] validate.sh:2470 label=seq
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2470 label=seq
hermit record start --data-dir "$RECORDING_DIR" -- /bin/cat README.md # [both: record] validate.sh:2472 label=cat
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2472 label=cat
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/wc -c README.md # [both: record] validate.sh:2474 label=wc
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2474 label=wc
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/head -n 3 README.md # [both: record] validate.sh:2476 label=head
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2476 label=head
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/base64 README.md # [both: record] validate.sh:2478 label=base64
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2478 label=base64
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/base32 README.md # [both: record] validate.sh:2480 label=base32
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2480 label=base32
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/id -u # [both: record] validate.sh:2482 label=id
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2482 label=id
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; printf "alpha 2\nbeta 3\nalpha 5\n" | awk "\$1 == \"alpha\" { sum += \$2 } END { print sum }" | diff -u <(printf "7\n") -; printf "awk-ok\n"' # [both: record] validate.sh:2529 label=awk
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2529 label=awk
hermit record start --data-dir "$RECORDING_DIR" -- make --version # [both: record] validate.sh:2647 label=make
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2647 label=make
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/ar --version # [both: record] validate.sh:2649 label=ar
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2649 label=ar
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/as --version # [both: record] validate.sh:2651 label=as
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2651 label=as
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/ld --version # [both: record] validate.sh:2653 label=ld
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2653 label=ld
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/nm --version # [both: record] validate.sh:2655 label=nm
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2655 label=nm
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/objcopy --version # [both: record] validate.sh:2657 label=objcopy
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2657 label=objcopy
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/objdump --version # [both: record] validate.sh:2659 label=objdump
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2659 label=objdump
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/ranlib --version # [both: record] validate.sh:2661 label=ranlib
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2661 label=ranlib
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/readelf --version # [both: record] validate.sh:2663 label=readelf
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2663 label=readelf
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/size --version # [both: record] validate.sh:2665 label=size
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2665 label=size
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/strip --version # [both: record] validate.sh:2667 label=strip
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2667 label=strip
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/addr2line --version # [both: record] validate.sh:2669 label=addr2line
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2669 label=addr2line
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/c++filt --version # [both: record] validate.sh:2671 label=c++filt
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2671 label=c++filt
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/elfedit --version # [both: record] validate.sh:2673 label=elfedit
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2673 label=elfedit
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/gprof --version # [both: record] validate.sh:2675 label=gprof
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2675 label=gprof
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'printf "beta\nalpha\nalpha\n" | sort' # [both: record] validate.sh:2738 label=sort
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2738 label=sort
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; printf "alpha\nalpha\nbeta\nbeta\ngamma\n" | uniq -d | diff -u <(printf "alpha\nbeta\n") -; printf "uniq-ok\n"' # [both: record] validate.sh:2741 label=uniq
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2741 label=uniq
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'printf "Hermit\n" | tr "[:upper:]" "[:lower:]"' # [both: record] validate.sh:2744 label=tr
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2744 label=tr
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; printf "one:two:three\nfour:five:six\n" | cut -d: -f2 | diff -u <(printf "two\nfive\n") -; printf "cut-ok\n"' # [both: record] validate.sh:2747 label=cut
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2747 label=cut
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'printf "tee-through-hermit\n" | tee /dev/null' # [both: record] validate.sh:2750 label=tee
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2750 label=tee
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; paste -d: <(printf "alpha\nbeta\n") <(printf "1\n2\n") | diff -u <(printf "alpha:1\nbeta:2\n") -; printf "paste-ok\n"' # [both: record] validate.sh:2753 label=paste
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2753 label=paste
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; comm -12 <(printf "alpha\nbeta\n") <(printf "beta\ngamma\n") | diff -u <(printf "beta\n") -; printf "comm-ok\n"' # [both: record] validate.sh:2756 label=comm
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2756 label=comm
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; join <(printf "1 alpha\n2 beta\n") <(printf "1 one\n2 two\n") | diff -u <(printf "1 alpha one\n2 beta two\n") -; printf "join-ok\n"' # [both: record] validate.sh:2759 label=join
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2759 label=join
hermit record start --data-dir "$RECORDING_DIR" -- find /etc -maxdepth 1 # [both: record] validate.sh:2762 label=find
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2762 label=find
hermit record start --data-dir "$RECORDING_DIR" -- stat -c '%n %s %f' /etc/hostname # [both: record] validate.sh:2765 label=stat
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2765 label=stat
hermit record start --data-dir "$RECORDING_DIR" -- file /bin/sh # [both: record] validate.sh:2767 label=file
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2767 label=file
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/basename /usr/local/bin/hermit # [both: record] validate.sh:2769 label=basename
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2769 label=basename
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/dirname /usr/local/bin/hermit # [both: record] validate.sh:2771 label=dirname
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2771 label=dirname
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/env -i HERMIT_COMPAT=env /usr/bin/env # [both: record] validate.sh:2773 label=env
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2773 label=env
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/env -i HERMIT_COMPAT=printenv /usr/bin/printenv HERMIT_COMPAT # [both: record] validate.sh:2775 label=printenv
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2775 label=printenv
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/uname -sr # [both: record] validate.sh:2778 label=uname
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2778 label=uname
hermit record start --data-dir "$RECORDING_DIR" -- factor 42 # [both: record] validate.sh:2780 label=factor
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2780 label=factor
hermit record start --data-dir "$RECORDING_DIR" -- expr 2 + 2 # [both: record] validate.sh:2782 label=expr
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2782 label=expr
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'printf "hermit-dd\n" | dd bs=1 count=10 status=none' # [both: record] validate.sh:2784 label=dd
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2784 label=dd
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/df -P / # [both: record] validate.sh:2787 label=df
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2787 label=df
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/du -sk README.md # [both: record] validate.sh:2789 label=du
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2789 label=du
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/hostname # [both: record] validate.sh:2791 label=hostname
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2791 label=hostname
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/whoami # [both: record] validate.sh:2805 label=whoami
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2805 label=whoami
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/groups # [both: record] validate.sh:2807 label=groups
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2807 label=groups
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'output=$(tty 2>&1); status=$?; printf "%s\n" "$output"; test "$status" -eq 1' # [both: record] validate.sh:2812 label=tty
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2812 label=tty
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/nproc # [both: record] validate.sh:2815 label=nproc
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2815 label=nproc
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/arch # [both: record] validate.sh:2817 label=arch
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2817 label=arch
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/realpath README.md # [both: record] validate.sh:2819 label=realpath
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2819 label=realpath
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/readlink -f README.md # [both: record] validate.sh:2821 label=readlink
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2821 label=readlink
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/sha256sum README.md # [both: record] validate.sh:2827 label=sha256sum
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2827 label=sha256sum
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/sha1sum README.md # [both: record] validate.sh:2829 label=sha1sum
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2829 label=sha1sum
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/md5sum README.md # [both: record] validate.sh:2831 label=md5sum
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2831 label=md5sum
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/sha224sum README.md # [both: record] validate.sh:2833 label=sha224sum
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2833 label=sha224sum
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/sha384sum README.md # [both: record] validate.sh:2835 label=sha384sum
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2835 label=sha384sum
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/sha512sum README.md # [both: record] validate.sh:2837 label=sha512sum
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2837 label=sha512sum
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/wc -l README.md # [both: record] validate.sh:2839 label=wc-lines
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2839 label=wc-lines
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'printf "alpha\nbeta\n" | nl -ba' # [both: record] validate.sh:2841 label=nl
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2841 label=nl
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'printf "a\tb\n" | expand -t 4' # [both: record] validate.sh:2844 label=expand
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2844 label=expand
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'printf "a   b\n" | unexpand -a -t 4' # [both: record] validate.sh:2847 label=unexpand
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2847 label=unexpand
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/test 42 -eq 42 # [both: record] validate.sh:2850 label=test
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2850 label=test
hermit record start --data-dir "$RECORDING_DIR" -- '/usr/bin/[' 42 -eq 42 ']' # [both: record] validate.sh:2852 label=bracket
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2852 label=bracket
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/printf '%s=%d\n' hermit 42 # [both: record] validate.sh:2854 label=printf
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2854 label=printf
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/pr -t README.md # [both: record] validate.sh:2856 label=pr
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2856 label=pr
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/ls -1 README.md # [both: record] validate.sh:2858 label=ls
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2858 label=ls
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'printf "one\ntwo\n" | /usr/bin/xargs -n1 /bin/echo' # [both: record] validate.sh:2860 label=xargs
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2860 label=xargs
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'printf "hermit\n" | /usr/bin/iconv -f UTF-8 -t UTF-8' # [both: record] validate.sh:2867 label=iconv
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2867 label=iconv
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/sleep 0 # [both: record] validate.sh:2870 label=sleep
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2870 label=sleep
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/stdbuf -o0 /usr/bin/printf 'stdbuf-ok\n' # [both: record] validate.sh:2872 label=stdbuf
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2872 label=stdbuf
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/nohup /bin/echo nohup-ok # [both: record] validate.sh:2875 label=nohup
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2875 label=nohup
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/nice -n 1 /bin/echo nice-ok # [both: record] validate.sh:2877 label=nice
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2877 label=nice
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/ionice -c 3 /bin/echo ionice-ok # [both: record] validate.sh:2879 label=ionice
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2879 label=ionice
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; taskset -p $$ >/dev/null; printf "taskset-ok\n"' # [both: record] validate.sh:2883 label=taskset
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2883 label=taskset
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; chrt -p $$ >/dev/null; printf "chrt-ok\n"' # [both: record] validate.sh:2887 label=chrt
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2887 label=chrt
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; f=$(mktemp); flock -x "$f" -c "printf \"flock-ok\\n\""; rm -f "$f"' # [both: record] validate.sh:2890 label=flock
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2890 label=flock
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; output=$(/usr/bin/logger --stderr --no-act -t hermit-compat logger-ok 2>&1); [[ $output == *"hermit-compat: logger-ok" ]]; printf "logger-ok\n"' # [both: record] validate.sh:2894 label=logger
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2894 label=logger
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/getopt -o ab: -- -a -b value # [both: record] validate.sh:2897 label=getopt
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2897 label=getopt
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; printf "alpha:1\nbeta:22\n" | column -t -s :' # [both: record] validate.sh:2899 label=column
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2899 label=column
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; printf "Hermit\n" | hexdump -C' # [both: record] validate.sh:2902 label=hexdump
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2902 label=hexdump
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; printf "Hermit\n" | xxd' # [both: record] validate.sh:2905 label=xxd
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2905 label=xxd
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; printf "\0Hermit\0" | strings -n 5' # [both: record] validate.sh:2908 label=strings
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2908 label=strings
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; printf "Hermit\n" | od -An -tx1' # [both: record] validate.sh:2911 label=od
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2911 label=od
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/sum README.md # [both: record] validate.sh:2914 label=sum
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2914 label=sum
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/cksum README.md # [both: record] validate.sh:2916 label=cksum
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2916 label=cksum
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/b2sum README.md # [both: record] validate.sh:2918 label=b2sum
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2918 label=b2sum
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; printf "alpha beta\nbeta gamma\n" | tsort' # [both: record] validate.sh:2920 label=tsort
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2920 label=tsort
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; printf "alpha beta\n" | ptx -f' # [both: record] validate.sh:2923 label=ptx
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2923 label=ptx
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/pinky -l root # [both: record] validate.sh:2926 label=pinky
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2926 label=pinky
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'if output=$(/usr/bin/logname 2>/dev/null); then test -n "$output"; printf "logname:login-present\n"; else printf "logname:no-login-record\n"; fi' # [both: record] validate.sh:2929 label=logname
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2929 label=logname
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/users # [both: record] validate.sh:2932 label=users
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2932 label=users
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/uptime -p # [both: record] validate.sh:2934 label=uptime
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2934 label=uptime
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; d=$(mktemp -d); printf "alpha\nbeta\n" >"$d/a"; cp "$d/a" "$d/b"; diff -u "$d/a" "$d/b"; rm -rf "$d"; printf "diff-ok\n"' # [both: record] validate.sh:3085 label=diff
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:3085 label=diff
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; printf "alpha\nbeta\ngamma\nalpha\n" | grep -nx alpha | diff -u <(printf "1:alpha\n4:alpha\n") -; printf "grep-ok\n"' # [both: record] validate.sh:3091 label=grep
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:3091 label=grep
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; printf "alpha\nbeta\ngamma\n" | egrep "^(alpha|gamma)$" | diff -u <(printf "alpha\ngamma\n") -; printf "egrep-ok\n"' # [both: record] validate.sh:3094 label=egrep
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:3094 label=egrep
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; printf "alpha.beta\nalphaXbeta\n" | fgrep "alpha.beta" | diff -u <(printf "alpha.beta\n") -; printf "fgrep-ok\n"' # [both: record] validate.sh:3097 label=fgrep
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:3097 label=fgrep
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; printf "alpha:12\nbeta:3\n" | sed -E "s/^([a-z]+):([0-9]+)$/\\2-\\1/" | diff -u <(printf "12-alpha\n3-beta\n") -; printf "sed-ok\n"' # [both: record] validate.sh:3100 label=sed
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:3100 label=sed
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; d=$(mktemp -d); printf "copy-data\n" >"$d/source"; cp "$d/source" "$d/copy"; cmp "$d/source" "$d/copy"; cat "$d/copy"; rm -rf "$d"' # [both: record] validate.sh:3106 label=cp
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:3106 label=cp
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; d=$(mktemp -d); printf "move-data\n" >"$d/source"; mv "$d/source" "$d/moved"; test ! -e "$d/source"; cat "$d/moved"; rm -rf "$d"' # [both: record] validate.sh:3109 label=mv
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:3109 label=mv
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; d=$(mktemp -d); printf "remove-data\n" >"$d/file"; rm "$d/file"; test ! -e "$d/file"; rmdir "$d"; printf "rm-ok\n"' # [both: record] validate.sh:3112 label=rm
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:3112 label=rm
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; d=$(mktemp -d); mkdir -p "$d/a/b"; test -d "$d/a/b"; printf "mkdir-ok\n"; rm -rf "$d"' # [both: record] validate.sh:3115 label=mkdir
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:3115 label=mkdir
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; d=$(mktemp -d); rmdir "$d"; test ! -e "$d"; printf "rmdir-ok\n"' # [both: record] validate.sh:3118 label=rmdir
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:3118 label=rmdir
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; f=$(mktemp); touch -t 200001010000 "$f"; stat -c "%Y %s" "$f"; rm -f "$f"' # [both: record] validate.sh:3121 label=touch
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:3121 label=touch
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; f=$(mktemp); printf "mode\n" >"$f"; chmod 640 "$f"; stat -c "%a" "$f"; rm -f "$f"' # [both: record] validate.sh:3124 label=chmod
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:3124 label=chmod
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/date -u +%Y-%m-%dT%H:%M:%SZ # [both: record] validate.sh:3133 label=date
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:3133 label=date
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/cal 1 2000 # [both: record] validate.sh:3135 label=cal
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:3135 label=cal
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -eu; yes hermit | head -n 3' # [both: record] validate.sh:3137 label=yes
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:3137 label=yes
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; printf "first\nsecond\nthird\n" | tac' # [both: record] validate.sh:3140 label=tac
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:3140 label=tac
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; printf "Hermit\ndeterminism\n" | rev' # [both: record] validate.sh:3143 label=rev
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:3143 label=rev
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; printf "abcdefghijklmnopqrstuvwxyz\n" | fold -w 8' # [both: record] validate.sh:3146 label=fold
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:3146 label=fold
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; printf "Hermit formats this deterministic paragraph into narrow lines for validation.\n" | fmt -w 24' # [both: record] validate.sh:3149 label=fmt
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:3149 label=fmt
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; output=$(printf "alpha\nbeta\ngamma\ndelta\n" | shuf | sort); test "$output" = "$(printf "alpha\nbeta\ndelta\ngamma\n")"; printf "shuf-ok\n"' # [both: record] validate.sh:3152 label=shuf
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:3152 label=shuf
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/numfmt --to=iec 1048576 # [both: record] validate.sh:3155 label=numfmt
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:3155 label=numfmt
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; d=$(mktemp -d); printf "one\ntwo\nthree\nfour\n" >"$d/input"; split -l 2 "$d/input" "$d/part-"; cat "$d"/part-*; rm -rf "$d"' # [both: record] validate.sh:3160 label=split
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:3160 label=split
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; d=$(mktemp -d); install -m 640 README.md "$d/copied"; stat -c "%a %s" "$d/copied"; rm -rf "$d"' # [both: record] validate.sh:3163 label=install
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:3163 label=install
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; p=$(mktemp -u); mkfifo "$p"; stat -c "%F" "$p"; rm -f "$p"' # [both: record] validate.sh:3166 label=mkfifo
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:3166 label=mkfifo
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; d=$(mktemp -d); printf "same\n" >"$d/a"; printf "same\n" >"$d/b"; cmp -s "$d/a" "$d/b"; printf "cmp-ok\n"; rm -rf "$d"' # [both: record] validate.sh:3170 label=cmp
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:3170 label=cmp
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/free -m # [both: record] validate.sh:3173 label=free
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:3173 label=free
hermit record start --data-dir "$RECORDING_DIR" -- /bin/echo "$SMOKE_MARKER" # [record/replay] validate.sh:772 hermit_record_replay_smoke record
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [record/replay] validate.sh:775 hermit_record_replay_smoke replay
hermit record start --verify --data-dir "$RECORDING_DIR" -- /bin/echo "$SUPER_RECORD_MARKER" # [record/replay] validate.sh:1037 ptrace-record-replay
hermit record start --verify -- /bin/true # [record/replay] validate.sh:3556/3772 envelope true rr
hermit record start --verify -- /bin/echo hermit-envelope # [record/replay] validate.sh:3556/3772 envelope echo rr
hermit record start --verify -- /bin/date -u +%Y # [record/replay] validate.sh:3556/3772 envelope date rr
hermit record start --verify --record-timeout=30 "--data-dir=$RECORDING_DIR" -- /bin/echo hello # [record/replay] hermit-cli/tests/record_replay.rs explicit-1
hermit record start --verify --record-timeout=30 "--data-dir=$RECORDING_DIR" -- /bin/sh -c '/usr/bin/yes | /usr/bin/head -n 1' # [record/replay] hermit-cli/tests/record_replay.rs explicit-3
hermit record start --verify --record-timeout=30 "--data-dir=$RECORDING_DIR" -- /bin/sh -c 'printf '"'"'b\na\n'"'"' | /usr/bin/sort' # [record/replay] hermit-cli/tests/record_replay.rs explicit-4
hermit record start --verify --record-timeout=30 "--data-dir=$RECORDING_DIR" -- /usr/bin/head -c 262144 /dev/zero # [record/replay] hermit-cli/tests/record_replay.rs explicit-5
hermit record start --verify --record-timeout=30 "--data-dir=$RECORDING_DIR" -- /bin/sh -c 'while :; do :; done' # [record/replay] hermit-cli/tests/record_replay.rs explicit-9
# Compression/archive
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'bzip2 -c README.md | sha256sum' # [both: record] validate.sh:2681 label=bzip2
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2681 label=bzip2
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'gzip -cn README.md | sha256sum' # [both: record] validate.sh:2684 label=gzip
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2684 label=gzip
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'xz -c README.md | sha256sum' # [both: record] validate.sh:2687 label=xz
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2687 label=xz
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'zstd -q -c README.md | sha256sum' # [both: record] validate.sh:2690 label=zstd
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2690 label=zstd
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'set -euo pipefail; d=$(mktemp -d); printf "archive-data\n" >"$d/input"; touch -t 200001010000 "$d/input"; tar -cf "$d/archive.tar" -C "$d" input; tar -tf "$d/archive.tar"; rm -rf "$d"' # [both: record] validate.sh:3103 label=tar
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:3103 label=tar
# Language runtimes
hermit record start --data-dir "$RECORDING_DIR" -- lua -e 'print(42)' # [record/replay] validate.sh:2507 label=lua
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [record/replay] validate.sh:2507 label=lua
hermit record start --data-dir "$RECORDING_DIR" -- perl -e 'print 42, chr(10)' # [record/replay] validate.sh:2509 label=perl
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [record/replay] validate.sh:2509 label=perl
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'printf "6*7\n" | bc' # [record/replay] validate.sh:2511 label=bc
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [record/replay] validate.sh:2511 label=bc
hermit record start --data-dir "$RECORDING_DIR" -- bash -c 'for i in 1 2 3; do echo "$i"; done' # [both: record] validate.sh:2544 label=bash
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2544 label=bash
hermit record start --data-dir "$RECORDING_DIR" -- java -Xint -XX:+UseSerialGC -XX:ActiveProcessorCount=1 -version # [both: record] validate.sh:2582 label=java
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2582 label=java
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/ruby --disable-gems -e 'values = (1..5).map { |value| value * value }; raise "unexpected squares" unless values == [1, 4, 9, 16, 25]; puts values.join(",")' # [both: record] validate.sh:2586 label=ruby
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2586 label=ruby
hermit record start --data-dir "$RECORDING_DIR" -- /bin/node -e 'console.log(42)' # [both: record] validate.sh:2592 label=node
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2592 label=node
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/python3 -c 'print(42)' # [both: record] validate.sh:2596 label=python3
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2596 label=python3
hermit record start --data-dir "$RECORDING_DIR" -- gcc --version # [record/replay] validate.sh:2642 label=gcc
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [record/replay] validate.sh:2642 label=gcc
hermit record start --data-dir "$RECORDING_DIR" -- g++ --version # [both: record] validate.sh:2645 label=g++
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2645 label=g++
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/cpp --version # [both: record] validate.sh:2677 label=cpp
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2677 label=cpp
hermit record start --data-dir "$RECORDING_DIR" -- /usr/bin/gcov --version # [both: record] validate.sh:2679 label=gcov
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2679 label=gcov
hermit record start --verify --record-timeout=30 "--data-dir=$RECORDING_DIR" -- /bin/bash -c 'set -euo pipefail; root=/tmp/hermit-record-mkdir-side-effect; rm -rf "$root"; mkdir "$root"; rmdir "$root"' # [record/replay] hermit-cli/tests/record_replay.rs explicit-2
hermit record start --verify --record-timeout=30 "--data-dir=$RECORDING_DIR" -- /usr/bin/node -e 'console.log(42)' # [record/replay] hermit-cli/tests/record_replay.rs explicit-7
# Applications
hermit record start --data-dir "$RECORDING_DIR" -- sqlite3 :memory: 'CREATE TABLE values_under_test(value INTEGER NOT NULL); WITH RECURSIVE sequence(value) AS (VALUES(1) UNION ALL SELECT value + 1 FROM sequence WHERE value < 100) INSERT INTO values_under_test SELECT value FROM sequence; SELECT count(*), sum(value) FROM values_under_test;' # [both: record] validate.sh:2532 label=sqlite3
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2532 label=sqlite3
hermit record start --data-dir "$RECORDING_DIR" -- /usr/local/bin/git.meta.real --version # [both: record] validate.sh:2624 label=git
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2624 label=git
hermit record start --data-dir "$RECORDING_DIR" -- openssl dgst -sha256 /etc/hostname # [both: record] validate.sh:2736 label=openssl
hermit replay --autopilot --data-dir "$RECORDING_DIR" # [both: replay] validate.sh:2736 label=openssl
hermit record start --verify --record-timeout=30 "--data-dir=$RECORDING_DIR" -- /usr/bin/curl --version # [record/replay] hermit-cli/tests/record_replay.rs explicit-6
hermit record start --verify --record-timeout=30 "--data-dir=$RECORDING_DIR" -- /usr/bin/sqlite3 :memory: 'SELECT 1+1;' # [record/replay] hermit-cli/tests/record_replay.rs explicit-8
# Regression tests
hermit record start --verify --record-timeout=30 "--data-dir=$RECORDING_DIR" -- "$CARGO_TARGET_TMPDIR/record-replay/c_getsockopt_null" # [record/replay] hermit-cli/tests/record_replay.rs workload=c_getsockopt_null
hermit record start --verify --record-timeout=30 "--data-dir=$RECORDING_DIR" -- "$CARGO_TARGET_TMPDIR/record-replay/c_setsockopt_replay" # [record/replay] hermit-cli/tests/record_replay.rs workload=c_setsockopt_replay
hermit record start --verify --record-timeout=30 "--data-dir=$RECORDING_DIR" -- "$CARGO_TARGET_TMPDIR/record-replay/c_record_replay_fd_close" # [record/replay] hermit-cli/tests/record_replay.rs workload=c_record_replay_fd_close
hermit record start --verify --record-timeout=30 "--data-dir=$RECORDING_DIR" -- "$CARGO_TARGET_TMPDIR/record-replay/c_sigpipe_siginfo" # [record/replay] hermit-cli/tests/record_replay.rs workload=c_sigpipe_siginfo
hermit record start --verify --record-timeout=30 "--data-dir=$RECORDING_DIR" -- "$CARGO_TARGET_TMPDIR/record-replay/c_pidfd_open_self" # [record/replay] hermit-cli/tests/record_replay.rs workload=c_pidfd_open_self
hermit record start --verify --record-timeout=30 "--data-dir=$RECORDING_DIR" -- "$CARGO_TARGET_TMPDIR/record-replay/c_pidfd_poll_self" # [record/replay] hermit-cli/tests/record_replay.rs workload=c_pidfd_poll_self
hermit record start --verify --record-timeout=30 "--data-dir=$RECORDING_DIR" -- "$CARGO_TARGET_TMPDIR/record-replay/rustbin_clock_total_order" # [record/replay] hermit-cli/tests/record_replay.rs workload=rustbin_clock_total_order
hermit record start --verify --record-timeout=30 "--data-dir=$RECORDING_DIR" -- "$CARGO_TARGET_TMPDIR/record-replay/rustbin_exit_group" # [record/replay] hermit-cli/tests/record_replay.rs workload=rustbin_exit_group
hermit record start --verify --record-timeout=30 "--data-dir=$RECORDING_DIR" -- "$CARGO_TARGET_TMPDIR/record-replay/rustbin_sched_yield" # [record/replay] hermit-cli/tests/record_replay.rs workload=rustbin_sched_yield
hermit record start --verify --record-timeout=30 "--data-dir=$RECORDING_DIR" -- "$CARGO_TARGET_TMPDIR/record-replay/rustbin_futex_timeout" # [record/replay] hermit-cli/tests/record_replay.rs workload=rustbin_futex_timeout
hermit record start --verify --record-timeout=30 "--data-dir=$RECORDING_DIR" -- "$CARGO_TARGET_TMPDIR/record-replay/rustbin_futex_wait_child" # [record/replay] hermit-cli/tests/record_replay.rs workload=rustbin_futex_wait_child
hermit record start --verify --record-timeout=30 "--data-dir=$RECORDING_DIR" -- "$CARGO_TARGET_TMPDIR/record-replay/rustbin_futex_wake_some" # [record/replay] hermit-cli/tests/record_replay.rs workload=rustbin_futex_wake_some
hermit record start --verify --record-timeout=30 "--data-dir=$RECORDING_DIR" -- "$CARGO_TARGET_TMPDIR/record-replay/rustbin_heap_ptrs" # [record/replay] hermit-cli/tests/record_replay.rs workload=rustbin_heap_ptrs
hermit record start --verify --record-timeout=30 "--data-dir=$RECORDING_DIR" -- "$CARGO_TARGET_TMPDIR/record-replay/rustbin_print_nanosleep_race" # [record/replay] hermit-cli/tests/record_replay.rs workload=rustbin_print_nanosleep_race
hermit record start --verify --record-timeout=30 "--data-dir=$RECORDING_DIR" -- "$CARGO_TARGET_TMPDIR/record-replay/rustbin_nanosleep" # [record/replay] hermit-cli/tests/record_replay.rs workload=rustbin_nanosleep
hermit record start --verify --record-timeout=30 "--data-dir=$RECORDING_DIR" -- "$CARGO_TARGET_TMPDIR/record-replay/rustbin_pipe_basics" # [record/replay] hermit-cli/tests/record_replay.rs workload=rustbin_pipe_basics
hermit record start --verify --record-timeout=30 "--data-dir=$RECORDING_DIR" -- "$CARGO_TARGET_TMPDIR/record-replay/rustbin_poll" # [record/replay] hermit-cli/tests/record_replay.rs workload=rustbin_poll
hermit record start --verify --record-timeout=30 "--data-dir=$RECORDING_DIR" -- "$CARGO_TARGET_TMPDIR/record-replay/rustbin_poll_spin" # [record/replay] hermit-cli/tests/record_replay.rs workload=rustbin_poll_spin
hermit record start --verify --record-timeout=30 "--data-dir=$RECORDING_DIR" -- "$CARGO_TARGET_TMPDIR/record-replay/rustbin_rdtsc" # [record/replay] hermit-cli/tests/record_replay.rs workload=rustbin_rdtsc
hermit record start --verify --record-timeout=30 "--data-dir=$RECORDING_DIR" -- "$CARGO_TARGET_TMPDIR/record-replay/rustbin_stack_ptr" # [record/replay] hermit-cli/tests/record_replay.rs workload=rustbin_stack_ptr
hermit record start --verify --record-timeout=30 "--data-dir=$RECORDING_DIR" -- "$CARGO_TARGET_TMPDIR/record-replay/rustbin_thread_random" # [record/replay] hermit-cli/tests/record_replay.rs workload=rustbin_thread_random
cargo test -p hermit --test aio_nr_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/aio_nr_determinism.rs
cargo test -p hermit --test aio_nr_determinism aio_nr_consumers_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/aio_nr_determinism.rs::aio_nr_consumers_verify
cargo test -p hermit --test analyze -- --include-ignored --test-threads=1 # [run] all tests in hermit-cli/tests/analyze.rs
cargo test -p hermit --test analyze analyze_hello_race -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/analyze.rs::analyze_hello_race
cargo test -p hermit --test analyze analyze_racewrite_nostdlib -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/analyze.rs::analyze_racewrite_nostdlib
cargo test -p hermit --test analyze analyze_nanosleep_threads_rejects_indistinguishable_baseline -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/analyze.rs::analyze_nanosleep_threads_rejects_indistinguishable_baseline
cargo test -p hermit --test app_strict_verify -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/app_strict_verify.rs
cargo test -p hermit --test app_strict_verify curl_version_is_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/app_strict_verify.rs::curl_version_is_deterministic_under_strict_verify
cargo test -p hermit --test app_strict_verify nginx_version_is_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/app_strict_verify.rs::nginx_version_is_deterministic_under_strict_verify
cargo test -p hermit --test app_strict_verify redis_server_version_is_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/app_strict_verify.rs::redis_server_version_is_deterministic_under_strict_verify
cargo test -p hermit --test app_strict_verify java_version_is_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/app_strict_verify.rs::java_version_is_deterministic_under_strict_verify
cargo test -p hermit --test app_strict_verify go_hello_is_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/app_strict_verify.rs::go_hello_is_deterministic_under_strict_verify
cargo test -p hermit --test app_strict_verify go_goroutines_are_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/app_strict_verify.rs::go_goroutines_are_deterministic_under_strict_verify
cargo test -p hermit --test app_strict_verify java_hello_is_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/app_strict_verify.rs::java_hello_is_deterministic_under_strict_verify
cargo test -p hermit --test app_strict_verify java_threads_are_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/app_strict_verify.rs::java_threads_are_deterministic_under_strict_verify
cargo test -p hermit --test app_strict_verify go_version_is_l1_deterministic_under_strict -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/app_strict_verify.rs::go_version_is_l1_deterministic_under_strict
cargo test -p hermit --test app_strict_verify javac_is_l1_deterministic_under_strict -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/app_strict_verify.rs::javac_is_l1_deterministic_under_strict
cargo test -p hermit --test arbitrary_binaries -- --include-ignored --test-threads=1 # [both/mixed] all tests in hermit-cli/tests/arbitrary_binaries.rs
cargo test -p hermit --test arbitrary_binaries run_arbitrary_binary_matrix -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/arbitrary_binaries.rs::run_arbitrary_binary_matrix
cargo test -p hermit --test arbitrary_binaries record_replay_stable_arbitrary_binaries -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/arbitrary_binaries.rs::record_replay_stable_arbitrary_binaries
cargo test -p hermit --test arbitrary_binaries arbitrary_binary_commands_are_bounded -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/arbitrary_binaries.rs::arbitrary_binary_commands_are_bounded
cargo test -p hermit --test arbitrary_binaries arbitrary_binary_lists_are_curated_for_ci -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/arbitrary_binaries.rs::arbitrary_binary_lists_are_curated_for_ci
cargo test -p hermit --test arch_prctl -- --include-ignored --test-threads=1 # [both/mixed] all tests in hermit-cli/tests/arch_prctl.rs
cargo test -p hermit --test arch_prctl arch_prctl_controls_verify_in_run_and_record_modes -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/arch_prctl.rs::arch_prctl_controls_verify_in_run_and_record_modes
cargo test -p hermit --test arch_status_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/arch_status_determinism.rs
cargo test -p hermit --test arch_status_determinism arch_status_consumers_are_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/arch_status_determinism.rs::arch_status_consumers_are_deterministic_under_strict_verify
cargo test -p hermit --test block_inflight_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/block_inflight_determinism.rs
cargo test -p hermit --test block_inflight_determinism block_inflight_consumers_are_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/block_inflight_determinism.rs::block_inflight_consumers_are_deterministic_under_strict_verify
cargo test -p hermit --test btrfs_commit_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/btrfs_commit_determinism.rs
cargo test -p hermit --test btrfs_commit_determinism btrfs_commit_stats_consumers_are_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/btrfs_commit_determinism.rs::btrfs_commit_stats_consumers_are_deterministic_under_strict_verify
cargo test -p hermit --test btrfs_pinned_bytes_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/btrfs_pinned_bytes_determinism.rs
cargo test -p hermit --test btrfs_pinned_bytes_determinism btrfs_pinned_space_consumers_are_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/btrfs_pinned_bytes_determinism.rs::btrfs_pinned_space_consumers_are_deterministic_under_strict_verify
cargo test -p hermit --test btrfs_reservation_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/btrfs_reservation_determinism.rs
cargo test -p hermit --test btrfs_reservation_determinism btrfs_reservation_consumers_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/btrfs_reservation_determinism.rs::btrfs_reservation_consumers_verify
cargo test -p hermit --test btrfs_reserved_bytes_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/btrfs_reserved_bytes_determinism.rs
cargo test -p hermit --test btrfs_reserved_bytes_determinism btrfs_reserved_bytes_consumers_are_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/btrfs_reserved_bytes_determinism.rs::btrfs_reserved_bytes_consumers_are_deterministic_under_strict_verify
cargo test -p hermit --test buddyinfo_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/buddyinfo_determinism.rs
cargo test -p hermit --test buddyinfo_determinism buddyinfo_consumers_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/buddyinfo_determinism.rs::buddyinfo_consumers_verify
cargo test -p hermit --test chaos_sched_yield_progress -- --include-ignored --test-threads=1 # [both/mixed] all tests in hermit-cli/tests/chaos_sched_yield_progress.rs
cargo test -p hermit --test chaos_sched_yield_progress chaos_sched_yield_makes_progress_without_timer_preemption -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/chaos_sched_yield_progress.rs::chaos_sched_yield_makes_progress_without_timer_preemption
cargo test -p hermit --test chaos_sched_yield_progress strict_sched_yield_is_deterministic -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/chaos_sched_yield_progress.rs::strict_sched_yield_is_deterministic
cargo test -p hermit --test chaos_sched_yield_progress strict_vfork_child_sched_yield_is_deterministic -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/chaos_sched_yield_progress.rs::strict_vfork_child_sched_yield_is_deterministic
cargo test -p hermit --test chaos_sched_yield_progress preemption_replay_preserves_vfork_sched_yield_progress -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/chaos_sched_yield_progress.rs::preemption_replay_preserves_vfork_sched_yield_progress
cargo test -p hermit --test chaos_stress_pmu_detection -- --include-ignored --test-threads=1 # [run] all tests in hermit-cli/tests/chaos_stress_pmu_detection.rs
cargo test -p hermit --test chaos_stress_pmu_detection detects_capable_host -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/chaos_stress_pmu_detection.rs::detects_capable_host
cargo test -p hermit --test chaos_stress_pmu_detection detects_legacy_hardware_labelled_host -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/chaos_stress_pmu_detection.rs::detects_legacy_hardware_labelled_host
cargo test -p hermit --test chaos_stress_pmu_detection rejects_not_supported_counter -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/chaos_stress_pmu_detection.rs::rejects_not_supported_counter
cargo test -p hermit --test chaos_stress_pmu_detection rejects_not_counted_counter -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/chaos_stress_pmu_detection.rs::rejects_not_counted_counter
cargo test -p hermit --test chaos_stress_pmu_detection rejects_perf_stat_failure -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/chaos_stress_pmu_detection.rs::rejects_perf_stat_failure
cargo test -p hermit --test chaos_stress_pmu_detection rejects_missing_perf_binary -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/chaos_stress_pmu_detection.rs::rejects_missing_perf_binary
cargo test -p hermit --test cli -- --include-ignored --test-threads=1 # [both/mixed] all tests in hermit-cli/tests/cli.rs
cargo test -p hermit --test cli top_level_help_lists_user_facing_commands -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::top_level_help_lists_user_facing_commands
cargo test -p hermit --test cli bisect_help_describes_schedule_endpoints -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::bisect_help_describes_schedule_endpoints
cargo test -p hermit --test cli replay_help_accepts_optional_recording_id -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::replay_help_accepts_optional_recording_id
cargo test -p hermit --test cli run_help_exposes_determinism_modes -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_help_exposes_determinism_modes
cargo test -p hermit --test cli run_strict_flag_is_accepted_and_runs -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_strict_flag_is_accepted_and_runs
cargo test -p hermit --test cli verify_verbose_requires_verify -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::verify_verbose_requires_verify
cargo test -p hermit --test cli run_rejects_unknown_backends_during_argument_parsing -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_rejects_unknown_backends_during_argument_parsing
cargo test -p hermit --test cli run_dbi_executes_integrated_backend -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_dbi_executes_integrated_backend
cargo test -p hermit --test cli run_ptrace_verify_reemits_unsupported_syscall_warning -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_ptrace_verify_reemits_unsupported_syscall_warning
cargo test -p hermit --test cli run_dbi_aggregates_unsupported_syscalls_and_strict_rejects_them -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_dbi_aggregates_unsupported_syscalls_and_strict_rejects_them
cargo test -p hermit --test cli run_dbi_strict_returns_with_blocked_stdin_source -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_dbi_strict_returns_with_blocked_stdin_source
cargo test -p hermit --test cli run_liteinst_verifies_detcore_backend -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_liteinst_verifies_detcore_backend
cargo test -p hermit --test cli run_dbi_keeps_diagnostics_out_of_guest_stderr -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_dbi_keeps_diagnostics_out_of_guest_stderr
cargo test -p hermit --test cli run_dbi_verifies_application_mmap -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_dbi_verifies_application_mmap
cargo test -p hermit --test cli run_dbi_verifies_process_wait_lifecycle -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_dbi_verifies_process_wait_lifecycle
cargo test -p hermit --test cli run_dbi_virtualizes_process_identities -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_dbi_virtualizes_process_identities
cargo test -p hermit --test cli run_dbi_verifies_shell_process_lifecycle -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_dbi_verifies_shell_process_lifecycle
cargo test -p hermit --test cli run_dbi_verifies_pipe_backpressure -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_dbi_verifies_pipe_backpressure
cargo test -p hermit --test cli run_dbi_recovers_after_failed_exec -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_dbi_recovers_after_failed_exec
cargo test -p hermit --test cli run_dbi_rejects_unfollowed_execveat -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_dbi_rejects_unfollowed_execveat
cargo test -p hermit --test cli run_kvm_executes_dynamic_guest -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_kvm_executes_dynamic_guest
cargo test -p hermit --test cli run_kvm_resolves_bare_program_from_guest_path -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_kvm_resolves_bare_program_from_guest_path
cargo test -p hermit --test cli run_kvm_propagates_explicit_environment -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_kvm_propagates_explicit_environment
cargo test -p hermit --test cli run_kvm_bash_process_substitution_is_deterministic -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_kvm_bash_process_substitution_is_deterministic
cargo test -p hermit --test cli run_kvm_cpuid_policy_is_deterministic -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_kvm_cpuid_policy_is_deterministic
cargo test -p hermit --test cli run_kvm_respects_workdir_for_relative_paths -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_kvm_respects_workdir_for_relative_paths
cargo test -p hermit --test cli run_kvm_lists_host_directory_metadata -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_kvm_lists_host_directory_metadata
cargo test -p hermit --test cli run_kvm_reads_host_file -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_kvm_reads_host_file
cargo test -p hermit --test cli run_kvm_reads_standard_input -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_kvm_reads_standard_input
cargo test -p hermit --test cli run_kvm_f_getfl_and_reads_standard_input -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_kvm_f_getfl_and_reads_standard_input
cargo test -p hermit --test cli run_kvm_verify_f_getfl_with_isolated_standard_input -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_kvm_verify_f_getfl_with_isolated_standard_input
cargo test -p hermit --test cli run_kvm_verify_isolates_standard_input -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_kvm_verify_isolates_standard_input
cargo test -p hermit --test cli run_kvm_preserves_closed_standard_input -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_kvm_preserves_closed_standard_input
cargo test -p hermit --test cli run_kvm_verify_does_not_write_to_standard_input -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_kvm_verify_does_not_write_to_standard_input
cargo test -p hermit --test cli run_kvm_counts_standard_input -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_kvm_counts_standard_input
cargo test -p hermit --test cli run_kvm_reports_hostname -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_kvm_reports_hostname
cargo test -p hermit --test cli run_kvm_pipe_pipe2_and_getgroups_round_trip -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_kvm_pipe_pipe2_and_getgroups_round_trip
cargo test -p hermit --test cli run_kvm_reports_fixed_supplementary_groups -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_kvm_reports_fixed_supplementary_groups
cargo test -p hermit --test cli namespace_only_rejects_every_explicit_backend -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::namespace_only_rejects_every_explicit_backend
cargo test -p hermit --test cli backend_accepted_in_global_position -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::backend_accepted_in_global_position
cargo test -p hermit --test cli sabre_backend_validation_honors_command_scope -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::sabre_backend_validation_honors_command_scope
cargo test -p hermit --test cli sabre_rpc_socket_is_hidden_from_proc_environ -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::sabre_rpc_socket_is_hidden_from_proc_environ
cargo test -p hermit --test cli global_position_rejects_unknown_backends -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::global_position_rejects_unknown_backends
cargo test -p hermit --test cli namespace_only_rejects_global_position_backend -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::namespace_only_rejects_global_position_backend
cargo test -p hermit --test cli incompatible_run_modes_fail_during_argument_parsing -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::incompatible_run_modes_fail_during_argument_parsing
cargo test -p hermit --test cli no_namespace_rejects_container_only_options -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::no_namespace_rejects_container_only_options
cargo test -p hermit --test cli no_namespace_runs_without_container_setup -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::no_namespace_runs_without_container_setup
cargo test -p hermit --test cli no_namespace_preserves_affinity_for_run_and_verify -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::no_namespace_preserves_affinity_for_run_and_verify
cargo test -p hermit --test cli record_help_lists_management_commands -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::record_help_lists_management_commands
cargo test -p hermit --test cli record_list_json_reports_an_empty_inventory -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::record_list_json_reports_an_empty_inventory
cargo test -p hermit --test cli run_rejects_invalid_programs_with_actionable_errors -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_rejects_invalid_programs_with_actionable_errors
cargo test -p hermit --test cli run_rejects_invalid_configuration_without_panicking -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_rejects_invalid_configuration_without_panicking
cargo test -p hermit --test cli run_rejects_a_missing_bind_source_before_mounting -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_rejects_a_missing_bind_source_before_mounting
cargo test -p hermit --test cli run_reports_denied_ptrace_and_seccomp_capabilities -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/cli.rs::run_reports_denied_ptrace_and_seccomp_capabilities
cargo test -p hermit --test clock_determinism -- --include-ignored --test-threads=1 # [run] all tests in hermit-cli/tests/clock_determinism.rs
cargo test -p hermit --test clock_determinism clock_apis_are_deterministic_across_five_runs -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/clock_determinism.rs::clock_apis_are_deterministic_across_five_runs
cargo test -p hermit --test clock_determinism strict_mode_eliminates_native_clock_nondeterminism -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/clock_determinism.rs::strict_mode_eliminates_native_clock_nondeterminism
cargo test -p hermit --test clock_discipline_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/clock_discipline_determinism.rs
cargo test -p hermit --test clock_discipline_determinism clock_discipline_and_kernel_log_are_host_independent -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/clock_discipline_determinism.rs::clock_discipline_and_kernel_log_are_host_independent
cargo test -p hermit --test command_strict_verify -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/command_strict_verify.rs
cargo test -p hermit --test command_strict_verify common_commands_are_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/command_strict_verify.rs::common_commands_are_deterministic_under_strict_verify
cargo test -p hermit --test command_strict_verify identity_commands_are_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/command_strict_verify.rs::identity_commands_are_deterministic_under_strict_verify
cargo test -p hermit --test command_strict_verify process_accounting_commands_are_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/command_strict_verify.rs::process_accounting_commands_are_deterministic_under_strict_verify
cargo test -p hermit --test command_strict_verify io_accounting_commands_are_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/command_strict_verify.rs::io_accounting_commands_are_deterministic_under_strict_verify
cargo test -p hermit --test command_strict_verify kernel_pseudofile_commands_are_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/command_strict_verify.rs::kernel_pseudofile_commands_are_deterministic_under_strict_verify
cargo test -p hermit --test command_strict_verify ionice_query_is_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/command_strict_verify.rs::ionice_query_is_deterministic_under_strict_verify
cargo test -p hermit --test command_strict_verify kernel_activity_commands_are_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/command_strict_verify.rs::kernel_activity_commands_are_deterministic_under_strict_verify
cargo test -p hermit --test command_strict_verify hardware_accounting_commands_are_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/command_strict_verify.rs::hardware_accounting_commands_are_deterministic_under_strict_verify
cargo test -p hermit --test command_strict_verify python_prlimit64_query_is_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/command_strict_verify.rs::python_prlimit64_query_is_deterministic_under_strict_verify
cargo test -p hermit --test command_strict_verify python_getrandom_is_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/command_strict_verify.rs::python_getrandom_is_deterministic_under_strict_verify
cargo test -p hermit --test compression -- --include-ignored --test-threads=1 # [run] all tests in hermit-cli/tests/compression.rs
cargo test -p hermit --test compression compression_tools_are_deterministic_under_strict_hermit -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/compression.rs::compression_tools_are_deterministic_under_strict_hermit
cargo test -p hermit --test copy_file_range_refusal -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/copy_file_range_refusal.rs
cargo test -p hermit --test copy_file_range_refusal copy_file_range_refusal_is_deterministic -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/copy_file_range_refusal.rs::copy_file_range_refusal_is_deterministic
cargo test -p hermit --test cppc_feedback_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/cppc_feedback_determinism.rs
cargo test -p hermit --test cppc_feedback_determinism cppc_feedback_consumers_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/cppc_feedback_determinism.rs::cppc_feedback_consumers_verify
cargo test -p hermit --test cpufreq_avg_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/cpufreq_avg_determinism.rs
cargo test -p hermit --test cpufreq_avg_determinism cpufreq_average_consumers_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/cpufreq_avg_determinism.rs::cpufreq_average_consumers_verify
cargo test -p hermit --test cpuidle_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/cpuidle_determinism.rs
cargo test -p hermit --test cpuidle_determinism cpuidle_counter_consumers_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/cpuidle_determinism.rs::cpuidle_counter_consumers_verify
cargo test -p hermit --test dentry_state_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/dentry_state_determinism.rs
cargo test -p hermit --test dentry_state_determinism dentry_state_consumers_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/dentry_state_determinism.rs::dentry_state_consumers_verify
cargo test -p hermit --test epoll_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/epoll_determinism.rs
cargo test -p hermit --test epoll_determinism multiple_ready_fds_have_deterministic_ordering -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/epoll_determinism.rs::multiple_ready_fds_have_deterministic_ordering
cargo test -p hermit --test epoll_determinism edge_triggered_delivery_is_deterministic -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/epoll_determinism.rs::edge_triggered_delivery_is_deterministic
cargo test -p hermit --test epoll_determinism oneshot_delivery_and_rearming_are_deterministic -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/epoll_determinism.rs::oneshot_delivery_and_rearming_are_deterministic
cargo test -p hermit --test epoll_determinism mixed_fd_readiness_is_deterministic -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/epoll_determinism.rs::mixed_fd_readiness_is_deterministic
cargo test -p hermit --test epoll_determinism nested_epoll_delivery_is_deterministic -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/epoll_determinism.rs::nested_epoll_delivery_is_deterministic
cargo test -p hermit --test epoll_determinism notification_control_syscalls_are_deterministic -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/epoll_determinism.rs::notification_control_syscalls_are_deterministic
cargo test -p hermit --test epoll_determinism notification_control_syscalls_reach_strict_verify_l2 -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/epoll_determinism.rs::notification_control_syscalls_reach_strict_verify_l2
cargo test -p hermit --test epoll_determinism epoll_fd_supports_descriptor_table_ops -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/epoll_determinism.rs::epoll_fd_supports_descriptor_table_ops
cargo test -p hermit --test file_nr_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/file_nr_determinism.rs
cargo test -p hermit --test file_nr_determinism file_nr_consumers_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/file_nr_determinism.rs::file_nr_consumers_verify
cargo test -p hermit --test fp_reduction_determinism -- --include-ignored --test-threads=1 # [run] all tests in hermit-cli/tests/fp_reduction_determinism.rs
cargo test -p hermit --test fp_reduction_determinism native_parallel_fp_reduction_exposes_low_bit_variation -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/fp_reduction_determinism.rs::native_parallel_fp_reduction_exposes_low_bit_variation
cargo test -p hermit --test fp_reduction_determinism strict_parallel_fp_reduction_is_bit_identical -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/fp_reduction_determinism.rs::strict_parallel_fp_reduction_is_bit_identical
cargo test -p hermit --test futex2_refusal -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/futex2_refusal.rs
cargo test -p hermit --test futex2_refusal futex2_feature_probes_receive_deterministic_enosys -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/futex2_refusal.rs::futex2_feature_probes_receive_deterministic_enosys
cargo test -p hermit --test getitimer_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/getitimer_determinism.rs
cargo test -p hermit --test getitimer_determinism getitimer_tracks_logical_alarm_state -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/getitimer_determinism.rs::getitimer_tracks_logical_alarm_state
cargo test -p hermit --test hashseed_determinism -- --include-ignored --test-threads=1 # [run] all tests in hermit-cli/tests/hashseed_determinism.rs
cargo test -p hermit --test hashseed_determinism python_set_order_nondeterministic_natively_deterministic_under_hermit -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/hashseed_determinism.rs::python_set_order_nondeterministic_natively_deterministic_under_hermit
cargo test -p hermit --test hermit_modes -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/hermit_modes.rs
cargo test -p hermit --test hermit_modes default_mode_matrix -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/hermit_modes.rs::default_mode_matrix
cargo test -p hermit --test hermit_modes resource_syscalls_are_deterministic_across_five_runs -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/hermit_modes.rs::resource_syscalls_are_deterministic_across_five_runs
cargo test -p hermit --test hermit_modes default_cargo_bind_connect_race -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/hermit_modes.rs::default_cargo_bind_connect_race
cargo test -p hermit --test hermit_modes default_cargo_clock_total_order -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/hermit_modes.rs::default_cargo_clock_total_order
cargo test -p hermit --test hermit_modes default_minimal_hello -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/hermit_modes.rs::default_minimal_hello
cargo test -p hermit --test hermit_modes default_lit_networking -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/hermit_modes.rs::default_lit_networking
cargo test -p hermit --test hermit_modes default_exit_codes -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/hermit_modes.rs::default_exit_codes
cargo test -p hermit --test hermit_modes default_virtualized_uname -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/hermit_modes.rs::default_virtualized_uname
cargo test -p hermit --test hermit_modes default_cat_issue -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/hermit_modes.rs::default_cat_issue
cargo test -p hermit --test hermit_modes default_bind_mounts -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/hermit_modes.rs::default_bind_mounts
cargo test -p hermit --test hermit_modes default_preserved_tmpfs -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/hermit_modes.rs::default_preserved_tmpfs
cargo test -p hermit --test hermit_modes default_environment_selection -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/hermit_modes.rs::default_environment_selection
cargo test -p hermit --test hermit_modes no_hardware_minimal_hello_backtraces -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/hermit_modes.rs::no_hardware_minimal_hello_backtraces
cargo test -p hermit --test hermit_modes no_hardware_stacktrace_signal -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/hermit_modes.rs::no_hardware_stacktrace_signal
cargo test -p hermit --test hermit_modes strict_mode_matrix -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/hermit_modes.rs::strict_mode_matrix
cargo test -p hermit --test hermit_modes chaos_mode_matrix -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/hermit_modes.rs::chaos_mode_matrix
cargo test -p hermit --test hermit_modes verify_mode_matrix -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/hermit_modes.rs::verify_mode_matrix
cargo test -p hermit --test hermit_modes verify_captures_debug_logs_when_a_lower_level_is_requested -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/hermit_modes.rs::verify_captures_debug_logs_when_a_lower_level_is_requested
cargo test -p hermit --test hermit_modes verify_reports_stdout_divergence -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/hermit_modes.rs::verify_reports_stdout_divergence
cargo test -p hermit --test hermit_modes verify_reports_exit_status_divergence -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/hermit_modes.rs::verify_reports_exit_status_divergence
cargo test -p hermit --test hermit_modes verify_verbose_compares_the_full_trace -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/hermit_modes.rs::verify_verbose_compares_the_full_trace
cargo test -p hermit --test hermit_modes verify_honors_tmp_and_environment -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/hermit_modes.rs::verify_honors_tmp_and_environment
cargo test -p hermit --test hermit_modes hello_race_chaos_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/hermit_modes.rs::hello_race_chaos_verify
cargo test -p hermit --test host_kernel_probes -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/host_kernel_probes.rs
cargo test -p hermit --test host_kernel_probes host_kernel_probes_fall_back_deterministically -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/host_kernel_probes.rs::host_kernel_probes_fall_back_deterministically
cargo test -p hermit --test host_security_identity -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/host_security_identity.rs
cargo test -p hermit --test host_security_identity host_security_identity_probes_fall_back_deterministically -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/host_security_identity.rs::host_security_identity_probes_fall_back_deterministically
cargo test -p hermit --test inode_nr_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/inode_nr_determinism.rs
cargo test -p hermit --test inode_nr_determinism inode_nr_consumers_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/inode_nr_determinism.rs::inode_nr_consumers_verify
cargo test -p hermit --test integration_matrix -- --include-ignored --test-threads=1 # [run] all tests in hermit-cli/tests/integration_matrix.rs
cargo test -p hermit --test integration_matrix integration_matrix -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/integration_matrix.rs::integration_matrix
cargo test -p hermit --test ipc_determinism -- --include-ignored --test-threads=1 # [run] all tests in hermit-cli/tests/ipc_determinism.rs
cargo test -p hermit --test ipc_determinism ipc_patterns_are_deterministic_across_five_runs -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/ipc_determinism.rs::ipc_patterns_are_deterministic_across_five_runs
cargo test -p hermit --test irq_per_cpu_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/irq_per_cpu_determinism.rs
cargo test -p hermit --test irq_per_cpu_determinism irq_per_cpu_count_consumers_are_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/irq_per_cpu_determinism.rs::irq_per_cpu_count_consumers_are_deterministic_under_strict_verify
cargo test -p hermit --test kernel_keyring -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/kernel_keyring.rs
cargo test -p hermit --test kernel_keyring kernel_keyring_is_deterministically_unavailable -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/kernel_keyring.rs::kernel_keyring_is_deterministically_unavailable
cargo test -p hermit --test kernel_keyring kernel_keyring_passes_through_in_non_strict_mode -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/kernel_keyring.rs::kernel_keyring_passes_through_in_non_strict_mode
cargo test -p hermit --test key_users_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/key_users_determinism.rs
cargo test -p hermit --test key_users_determinism key_user_consumers_are_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/key_users_determinism.rs::key_user_consumers_are_deterministic_under_strict_verify
cargo test -p hermit --test language_runtime_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/language_runtime_determinism.rs
cargo test -p hermit --test language_runtime_determinism go_runtime_entropy_is_determinized -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/language_runtime_determinism.rs::go_runtime_entropy_is_determinized
cargo test -p hermit --test language_runtime_determinism ruby_runtime_entropy_is_determinized -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/language_runtime_determinism.rs::ruby_runtime_entropy_is_determinized
cargo test -p hermit --test language_runtime_determinism ruby_thread_prctls_are_supported_in_fail_closed_mode -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/language_runtime_determinism.rs::ruby_thread_prctls_are_supported_in_fail_closed_mode
cargo test -p hermit --test language_runtime_determinism node_runtime_entropy_is_determinized -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/language_runtime_determinism.rs::node_runtime_entropy_is_determinized
cargo test -p hermit --test language_runtime_determinism jvm_runtime_entropy_is_determinized -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/language_runtime_determinism.rs::jvm_runtime_entropy_is_determinized
cargo test -p hermit --test language_runtime_determinism ocaml_runtime_entropy_is_determinized -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/language_runtime_determinism.rs::ocaml_runtime_entropy_is_determinized
cargo test -p hermit --test language_runtime_determinism python_runtime_entropy_is_determinized -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/language_runtime_determinism.rs::python_runtime_entropy_is_determinized
cargo test -p hermit --test leveldb -- --include-ignored --test-threads=1 # [run] all tests in hermit-cli/tests/leveldb.rs
cargo test -p hermit --test leveldb focused_leveldb_tests_are_deterministic_under_strict -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/leveldb.rs::focused_leveldb_tests_are_deterministic_under_strict
cargo test -p hermit --test leveldb full_leveldb_suite_is_deterministic_under_strict -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/leveldb.rs::full_leveldb_suite_is_deterministic_under_strict
cargo test -p hermit --test leveldb leveldb_env_posix_is_deterministic_under_strict -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/leveldb.rs::leveldb_env_posix_is_deterministic_under_strict
cargo test -p hermit --test liteinst_advanced -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/liteinst_advanced.rs
cargo test -p hermit --test liteinst_advanced liteinst_detcore_strict_verify_micro_suite -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/liteinst_advanced.rs::liteinst_detcore_strict_verify_micro_suite
cargo test -p hermit --test liteinst_advanced liteinst_thread_clone_fails_closed_without_sigsys -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/liteinst_advanced.rs::liteinst_thread_clone_fails_closed_without_sigsys
cargo test -p hermit --test liteinst_advanced liteinst_fork_fails_closed_without_hanging -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/liteinst_advanced.rs::liteinst_fork_fails_closed_without_hanging
cargo test -p hermit --test liteinst_advanced liteinst_abnormal_exit_after_registration_does_not_hang -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/liteinst_advanced.rs::liteinst_abnormal_exit_after_registration_does_not_hang
cargo test -p hermit --test madvise -- --include-ignored --test-threads=1 # [both/mixed] all tests in hermit-cli/tests/madvise.rs
cargo test -p hermit --test madvise madvise_policy_verifies_in_run_record_and_kvm_modes -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/madvise.rs::madvise_policy_verifies_in_run_record_and_kvm_modes
cargo test -p hermit --test meminfo_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/meminfo_determinism.rs
cargo test -p hermit --test meminfo_determinism meminfo_fields_and_free_use_guest_memory -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/meminfo_determinism.rs::meminfo_fields_and_free_use_guest_memory
cargo test -p hermit --test mmap_determinism -- --include-ignored --test-threads=1 # [run] all tests in hermit-cli/tests/mmap_determinism.rs
cargo test -p hermit --test mmap_determinism multiple_mmap_addresses_are_deterministic -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/mmap_determinism.rs::multiple_mmap_addresses_are_deterministic
cargo test -p hermit --test mmap_determinism map_fixed_address_is_deterministic -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/mmap_determinism.rs::map_fixed_address_is_deterministic
cargo test -p hermit --test mmap_determinism brk_and_sbrk_addresses_are_deterministic -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/mmap_determinism.rs::brk_and_sbrk_addresses_are_deterministic
cargo test -p hermit --test mmap_determinism map_shared_address_is_deterministic -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/mmap_determinism.rs::map_shared_address_is_deterministic
cargo test -p hermit --test mmap_determinism mmap_reuses_unmapped_address_deterministically -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/mmap_determinism.rs::mmap_reuses_unmapped_address_deterministically
cargo test -p hermit --test mount_introspection -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/mount_introspection.rs
cargo test -p hermit --test mount_introspection mount_introspection_syscalls_fall_back_deterministically -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/mount_introspection.rs::mount_introspection_syscalls_fall_back_deterministically
cargo test -p hermit --test name_to_handle_refusal -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/name_to_handle_refusal.rs
cargo test -p hermit --test name_to_handle_refusal filesystem_handle_export_refusals_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/name_to_handle_refusal.rs::filesystem_handle_export_refusals_verify
cargo test -p hermit --test netlink_table_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/netlink_table_determinism.rs
cargo test -p hermit --test netlink_table_determinism netlink_table_consumers_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/netlink_table_determinism.rs::netlink_table_consumers_verify
cargo test -p hermit --test netns_cookie_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/netns_cookie_determinism.rs
cargo test -p hermit --test netns_cookie_determinism network_namespace_cookie_verifies_for_distinct_socket_programs -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/netns_cookie_determinism.rs::network_namespace_cookie_verifies_for_distinct_socket_programs
cargo test -p hermit --test node_vmstat_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/node_vmstat_determinism.rs
cargo test -p hermit --test node_vmstat_determinism node_vmstat_consumers_are_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/node_vmstat_determinism.rs::node_vmstat_consumers_are_deterministic_under_strict_verify
cargo test -p hermit --test numa_maps_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/numa_maps_determinism.rs
cargo test -p hermit --test numa_maps_determinism numa_maps_consumers_are_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/numa_maps_determinism.rs::numa_maps_consumers_are_deterministic_under_strict_verify
cargo test -p hermit --test optional_memory_features -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/optional_memory_features.rs
cargo test -p hermit --test optional_memory_features optional_memory_features_fall_back_deterministically -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/optional_memory_features.rs::optional_memory_features_fall_back_deterministically
cargo test -p hermit --test perf_event_refusal -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/perf_event_refusal.rs
cargo test -p hermit --test perf_event_refusal perf_event_refusals_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/perf_event_refusal.rs::perf_event_refusals_verify
cargo test -p hermit --test pidfd_creation -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/pidfd_creation.rs
cargo test -p hermit --test pidfd_creation pidfd_creation_is_tracked_across_descriptor_operations -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/pidfd_creation.rs::pidfd_creation_is_tracked_across_descriptor_operations
cargo test -p hermit --test ppoll_simulation -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/ppoll_simulation.rs
cargo test -p hermit --test ppoll_simulation ppoll_waits_use_nonblocking_probes_and_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/ppoll_simulation.rs::ppoll_waits_use_nonblocking_probes_and_verify
cargo test -p hermit --test prctl_dumpable_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/prctl_dumpable_determinism.rs
cargo test -p hermit --test prctl_dumpable_determinism dumpability_controls_are_deterministic -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/prctl_dumpable_determinism.rs::dumpability_controls_are_deterministic
cargo test -p hermit --test privileged_observation -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/privileged_observation.rs
cargo test -p hermit --test privileged_observation privileged_observation_is_refused_deterministically -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/privileged_observation.rs::privileged_observation_is_refused_deterministically
cargo test -p hermit --test proc_fd_link_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/proc_fd_link_determinism.rs
cargo test -p hermit --test proc_fd_link_determinism proc_fd_link_consumers_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/proc_fd_link_determinism.rs::proc_fd_link_consumers_verify
cargo test -p hermit --test proc_fd_link_determinism proc_fd_link_aliases_and_truncation_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/proc_fd_link_determinism.rs::proc_fd_link_aliases_and_truncation_verify
cargo test -p hermit --test proc_fdinfo_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/proc_fdinfo_determinism.rs
cargo test -p hermit --test proc_fdinfo_determinism proc_fdinfo_consumers_are_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/proc_fdinfo_determinism.rs::proc_fdinfo_consumers_are_deterministic_under_strict_verify
cargo test -p hermit --test proc_locks_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/proc_locks_determinism.rs
cargo test -p hermit --test proc_locks_determinism proc_locks_consumers_are_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/proc_locks_determinism.rs::proc_locks_consumers_are_deterministic_under_strict_verify
cargo test -p hermit --test process_isolation_refusals -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/process_isolation_refusals.rs
cargo test -p hermit --test process_isolation_refusals process_isolation_refusals_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/process_isolation_refusals.rs::process_isolation_refusals_verify
cargo test -p hermit --test procfs_determinism -- --include-ignored --test-threads=1 # [run] all tests in hermit-cli/tests/procfs_determinism.rs
cargo test -p hermit --test procfs_determinism proc_self_maps_is_deterministic -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/procfs_determinism.rs::proc_self_maps_is_deterministic
cargo test -p hermit --test procfs_determinism proc_self_stat_is_deterministic -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/procfs_determinism.rs::proc_self_stat_is_deterministic
cargo test -p hermit --test procfs_determinism proc_self_status_is_deterministic -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/procfs_determinism.rs::proc_self_status_is_deterministic
cargo test -p hermit --test procfs_determinism proc_self_cmdline_is_deterministic -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/procfs_determinism.rs::proc_self_cmdline_is_deterministic
cargo test -p hermit --test procfs_determinism proc_system_cpu_accounting_is_deterministic -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/procfs_determinism.rs::proc_system_cpu_accounting_is_deterministic
cargo test -p hermit --test procfs_determinism proc_vm_accounting_is_deterministic -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/procfs_determinism.rs::proc_vm_accounting_is_deterministic
cargo test -p hermit --test procfs_determinism proc_pid_stat_accounting_is_deterministic -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/procfs_determinism.rs::proc_pid_stat_accounting_is_deterministic
cargo test -p hermit --test procfs_determinism proc_pid_statm_accounting_is_deterministic -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/procfs_determinism.rs::proc_pid_statm_accounting_is_deterministic
cargo test -p hermit --test procfs_determinism proc_pid_status_accounting_is_deterministic -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/procfs_determinism.rs::proc_pid_status_accounting_is_deterministic
cargo test -p hermit --test procfs_determinism proc_diskstats_uses_synthetic_counters -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/procfs_determinism.rs::proc_diskstats_uses_synthetic_counters
cargo test -p hermit --test procfs_determinism proc_pid_io_uses_zero_counters -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/procfs_determinism.rs::proc_pid_io_uses_zero_counters
cargo test -p hermit --test procfs_determinism proc_cpuinfo_is_deterministic -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/procfs_determinism.rs::proc_cpuinfo_is_deterministic
cargo test -p hermit --test procfs_determinism proc_loadavg_uses_virtual_values -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/procfs_determinism.rs::proc_loadavg_uses_virtual_values
cargo test -p hermit --test procfs_determinism proc_uptime_uses_virtual_time -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/procfs_determinism.rs::proc_uptime_uses_virtual_time
cargo test -p hermit --test procfs_determinism proc_entropy_available_is_deterministic -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/procfs_determinism.rs::proc_entropy_available_is_deterministic
cargo test -p hermit --test procfs_determinism proc_pressure_uses_virtual_zero_values -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/procfs_determinism.rs::proc_pressure_uses_virtual_zero_values
cargo test -p hermit --test procfs_determinism proc_interrupt_accounting_is_deterministic -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/procfs_determinism.rs::proc_interrupt_accounting_is_deterministic
cargo test -p hermit --test procfs_determinism proc_schedstat_uses_virtual_zero_values -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/procfs_determinism.rs::proc_schedstat_uses_virtual_zero_values
cargo test -p hermit --test procfs_determinism proc_zoneinfo_uses_virtual_zero_values -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/procfs_determinism.rs::proc_zoneinfo_uses_virtual_zero_values
cargo test -p hermit --test procfs_determinism proc_rtc_tracks_custom_epoch_and_virtual_time -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/procfs_determinism.rs::proc_rtc_tracks_custom_epoch_and_virtual_time
cargo test -p hermit --test procfs_determinism proc_self_mountinfo_hides_private_temp_roots -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/procfs_determinism.rs::proc_self_mountinfo_hides_private_temp_roots
cargo test -p hermit --test procfs_determinism proc_random_uuid_is_deterministic -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/procfs_determinism.rs::proc_random_uuid_is_deterministic
cargo test -p hermit --test procfs_determinism proc_modules_are_deterministic -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/procfs_determinism.rs::proc_modules_are_deterministic
cargo test -p hermit --test procfs_determinism sysfs_numa_accounting_is_deterministic -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/procfs_determinism.rs::sysfs_numa_accounting_is_deterministic
cargo test -p hermit --test procfs_determinism sysfs_hwmon_input_is_deterministic_when_available -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/procfs_determinism.rs::sysfs_hwmon_input_is_deterministic_when_available
cargo test -p hermit --test procfs_positioned_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/procfs_positioned_determinism.rs
cargo test -p hermit --test procfs_positioned_determinism procfs_positioned_reads_are_mediated_and_deterministic -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/procfs_positioned_determinism.rs::procfs_positioned_reads_are_mediated_and_deterministic
cargo test -p hermit --test protocols_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/protocols_determinism.rs
cargo test -p hermit --test protocols_determinism protocol_consumers_are_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/protocols_determinism.rs::protocol_consumers_are_deterministic_under_strict_verify
cargo test -p hermit --test pselect6_simulation -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/pselect6_simulation.rs
cargo test -p hermit --test pselect6_simulation pselect6_preserves_kernel_abi_and_unblocks_scheduler -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/pselect6_simulation.rs::pselect6_preserves_kernel_abi_and_unblocks_scheduler
cargo test -p hermit --test ptrace_refusal -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/ptrace_refusal.rs
cargo test -p hermit --test ptrace_refusal guest_ptrace_refusals_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/ptrace_refusal.rs::guest_ptrace_refusals_verify
cargo test -p hermit --test pty_nr_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/pty_nr_determinism.rs
cargo test -p hermit --test pty_nr_determinism pty_nr_consumers_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/pty_nr_determinism.rs::pty_nr_consumers_verify
cargo test -p hermit --test python_stdlib -- --include-ignored --test-threads=1 # [run] all tests in hermit-cli/tests/python_stdlib.rs
cargo test -p hermit --test python_stdlib zero_case_module_is_rejected -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/python_stdlib.rs::zero_case_module_is_rejected
cargo test -p hermit --test python_stdlib strict_python_stdlib_is_deterministic -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/python_stdlib.rs::strict_python_stdlib_is_deterministic
cargo test -p hermit --test random_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/random_determinism.rs
cargo test -p hermit --test random_determinism random_sources_repeat_across_runs_and_change_with_seed -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/random_determinism.rs::random_sources_repeat_across_runs_and_change_with_seed
cargo test -p hermit --test random_determinism random_sources_are_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/random_determinism.rs::random_sources_are_deterministic_under_strict_verify
cargo test -p hermit --test random_uuid_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/random_uuid_determinism.rs
cargo test -p hermit --test random_uuid_determinism random_uuid_consumers_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/random_uuid_determinism.rs::random_uuid_consumers_verify
cargo test -p hermit --test rcx_canonicalization -- --include-ignored --test-threads=1 # [run] all tests in hermit-cli/tests/rcx_canonicalization.rs
cargo test -p hermit --test rcx_canonicalization rcx_r11_are_canonical_and_deterministic_under_strict -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/rcx_canonicalization.rs::rcx_r11_are_canonical_and_deterministic_under_strict
cargo test -p hermit --test record_replay -- --include-ignored --test-threads=1 # [both/mixed] all tests in hermit-cli/tests/record_replay.rs
cargo test -p hermit --test record_replay record_strict_direct_cli_records_and_replays_echo -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/record_replay.rs::record_strict_direct_cli_records_and_replays_echo
cargo test -p hermit --test record_replay record_replay_matrix -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/record_replay.rs::record_replay_matrix
cargo test -p hermit --test record_replay record_reopened_inherited_and_cloned_file_state -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/record_replay.rs::record_reopened_inherited_and_cloned_file_state
cargo test -p hermit --test record_replay record_find_directory_tree -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/record_replay.rs::record_find_directory_tree
cargo test -p hermit --test record_replay record_mkdir_and_rmdir_side_effects -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/record_replay.rs::record_mkdir_and_rmdir_side_effects
cargo test -p hermit --test record_replay record_nested_mkdir_side_effects -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/record_replay.rs::record_nested_mkdir_side_effects
cargo test -p hermit --test record_replay record_writable_filesystem_side_effects -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/record_replay.rs::record_writable_filesystem_side_effects
cargo test -p hermit --test record_replay record_mkfifo_in_replay_tmp -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/record_replay.rs::record_mkfifo_in_replay_tmp
cargo test -p hermit --test record_replay record_shell_forked_external_command -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/record_replay.rs::record_shell_forked_external_command
cargo test -p hermit --test record_replay record_shell_sigpipe_pipeline -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/record_replay.rs::record_shell_sigpipe_pipeline
cargo test -p hermit --test record_replay record_shell_pipeline_stdout_matches -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/record_replay.rs::record_shell_pipeline_stdout_matches
cargo test -p hermit --test record_replay record_large_captured_output_does_not_deadlock -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/record_replay.rs::record_large_captured_output_does_not_deadlock
cargo test -p hermit --test record_replay record_shell_command_substitution_stdout_matches -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/record_replay.rs::record_shell_command_substitution_stdout_matches
cargo test -p hermit --test record_replay record_shell_redirected_stdout_stays_hidden -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/record_replay.rs::record_shell_redirected_stdout_stays_hidden
cargo test -p hermit --test record_replay record_shell_original_output_aliases_and_swaps -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/record_replay.rs::record_shell_original_output_aliases_and_swaps
cargo test -p hermit --test record_replay record_curl_version -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/record_replay.rs::record_curl_version
cargo test -p hermit --test record_replay record_node_eventfd_epoll_sequence -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/record_replay.rs::record_node_eventfd_epoll_sequence
cargo test -p hermit --test record_replay record_sqlite_memory_query -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/record_replay.rs::record_sqlite_memory_query
cargo test -p hermit --test record_replay record_timeout_kills_guest_without_committing_partial_data -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/record_replay.rs::record_timeout_kills_guest_without_committing_partial_data
cargo test -p hermit --test record_replay record_timeout_fires_even_when_sigalrm_is_blocked -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/record_replay.rs::record_timeout_fires_even_when_sigalrm_is_blocked
cargo test -p hermit --test record_replay record_timeout_preserves_existing_last -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/record_replay.rs::record_timeout_preserves_existing_last
cargo test -p hermit --test record_replay record_timeout_terminates_descendant_processes -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/record_replay.rs::record_timeout_terminates_descendant_processes
cargo test -p hermit --test record_replay record_pidfd_open_modeled_descriptor_ops -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/record_replay.rs::record_pidfd_open_modeled_descriptor_ops
cargo test -p hermit --test redis_strict -- --include-ignored --test-threads=1 # [run] all tests in hermit-cli/tests/redis_strict.rs
cargo test -p hermit --test redis_strict redis_small_subset_is_deterministic_under_hermit -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/redis_strict.rs::redis_small_subset_is_deterministic_under_hermit
cargo test -p hermit --test redis_strict redis_persistence_restart_is_deterministic_under_hermit -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/redis_strict.rs::redis_persistence_restart_is_deterministic_under_hermit
cargo test -p hermit --test redis_strict redis_workload_refuses_to_control_a_preexisting_server -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/redis_strict.rs::redis_workload_refuses_to_control_a_preexisting_server
cargo test -p hermit --test redis_strict redis_source_build_and_extended_suite_under_strict_hermit -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/redis_strict.rs::redis_source_build_and_extended_suite_under_strict_hermit
cargo test -p hermit --test remap_file_pages_refusal -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/remap_file_pages_refusal.rs
cargo test -p hermit --test remap_file_pages_refusal remap_file_pages_refusals_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/remap_file_pages_refusal.rs::remap_file_pages_refusals_verify
cargo test -p hermit --test robust_list_queries -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/robust_list_queries.rs
cargo test -p hermit --test robust_list_queries robust_list_queries_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/robust_list_queries.rs::robust_list_queries_verify
cargo test -p hermit --test rr_suite -- --include-ignored --test-threads=1 # [run] all tests in hermit-cli/tests/rr_suite.rs
cargo test -p hermit --test rr_suite rr_scratch_directories_are_fresh_and_cleaned -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/rr_suite.rs::rr_scratch_directories_are_fresh_and_cleaned
cargo test -p hermit --test rr_suite rr_pause -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/rr_suite.rs::rr_pause
cargo test -p hermit --test sched_setattr_noop -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/sched_setattr_noop.rs
cargo test -p hermit --test sched_setattr_noop scheduler_policy_setters_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/sched_setattr_noop.rs::scheduler_policy_setters_verify
cargo test -p hermit --test scheduler_policy_queries -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/scheduler_policy_queries.rs
cargo test -p hermit --test scheduler_policy_queries scheduler_policy_queries_are_deterministic -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/scheduler_policy_queries.rs::scheduler_policy_queries_are_deterministic
cargo test -p hermit --test self_sched_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/self_sched_determinism.rs
cargo test -p hermit --test self_sched_determinism self_sched_consumers_are_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/self_sched_determinism.rs::self_sched_consumers_are_deterministic_under_strict_verify
cargo test -p hermit --test self_schedstat_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/self_schedstat_determinism.rs
cargo test -p hermit --test self_schedstat_determinism self_schedstat_consumers_are_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/self_schedstat_determinism.rs::self_schedstat_consumers_are_deterministic_under_strict_verify
cargo test -p hermit --test signal_determinism -- --include-ignored --test-threads=1 # [run] all tests in hermit-cli/tests/signal_determinism.rs
cargo test -p hermit --test signal_determinism sigalrm_itimer_delivery_is_deterministic -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/signal_determinism.rs::sigalrm_itimer_delivery_is_deterministic
cargo test -p hermit --test signal_determinism armed_itimer_is_discarded_on_process_exit -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/signal_determinism.rs::armed_itimer_is_discarded_on_process_exit
cargo test -p hermit --test signal_determinism signal_interrupts_emulated_blocking_read -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/signal_determinism.rs::signal_interrupts_emulated_blocking_read
cargo test -p hermit --test signal_determinism signal_restarts_emulated_blocking_read -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/signal_determinism.rs::signal_restarts_emulated_blocking_read
cargo test -p hermit --test signal_determinism signal_interrupts_poll_despite_sa_restart -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/signal_determinism.rs::signal_interrupts_poll_despite_sa_restart
cargo test -p hermit --test signal_determinism signal_interrupts_epoll_wait_despite_sa_restart -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/signal_determinism.rs::signal_interrupts_epoll_wait_despite_sa_restart
cargo test -p hermit --test signal_determinism signal_interrupts_rt_sigtimedwait_despite_sa_restart -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/signal_determinism.rs::signal_interrupts_rt_sigtimedwait_despite_sa_restart
cargo test -p hermit --test signal_determinism blocking_sigsuspend_releases_the_scheduler -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/signal_determinism.rs::blocking_sigsuspend_releases_the_scheduler
cargo test -p hermit --test signal_determinism signal_masks_survive_fork_and_clone -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/signal_determinism.rs::signal_masks_survive_fork_and_clone
cargo test -p hermit --test signal_determinism signal_handler_reentrance_is_deterministic -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/signal_determinism.rs::signal_handler_reentrance_is_deterministic
cargo test -p hermit --test signal_determinism alternate_signal_stack_is_preserved -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/signal_determinism.rs::alternate_signal_stack_is_preserved
cargo test -p hermit --test signal_determinism pending_signal_and_mask_survive_exec -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/signal_determinism.rs::pending_signal_and_mask_survive_exec
cargo test -p hermit --test smaps_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/smaps_determinism.rs
cargo test -p hermit --test smaps_determinism smaps_consumers_are_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/smaps_determinism.rs::smaps_consumers_are_deterministic_under_strict_verify
cargo test -p hermit --test smaps_rollup_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/smaps_rollup_determinism.rs
cargo test -p hermit --test smaps_rollup_determinism smaps_rollup_consumers_are_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/smaps_rollup_determinism.rs::smaps_rollup_consumers_are_deterministic_under_strict_verify
cargo test -p hermit --test so_incoming_cpu -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/so_incoming_cpu.rs
cargo test -p hermit --test so_incoming_cpu incoming_cpu_is_the_virtual_cpu_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/so_incoming_cpu.rs::incoming_cpu_is_the_virtual_cpu_under_strict_verify
cargo test -p hermit --test socket_cookie_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/socket_cookie_determinism.rs
cargo test -p hermit --test socket_cookie_determinism socket_cookies_verify_for_distinct_socket_families -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/socket_cookie_determinism.rs::socket_cookies_verify_for_distinct_socket_families
cargo test -p hermit --test socket_ioctl_timestamp_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/socket_ioctl_timestamp_determinism.rs
cargo test -p hermit --test socket_ioctl_timestamp_determinism socket_timestamp_ioctls_use_logical_time -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/socket_ioctl_timestamp_determinism.rs::socket_timestamp_ioctls_use_logical_time
cargo test -p hermit --test socket_timestamp_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/socket_timestamp_determinism.rs
cargo test -p hermit --test socket_timestamp_determinism socket_receive_timestamps_use_logical_time -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/socket_timestamp_determinism.rs::socket_receive_timestamps_use_logical_time
cargo test -p hermit --test sockstat_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/sockstat_determinism.rs
cargo test -p hermit --test sockstat_determinism sockstat_consumers_are_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/sockstat_determinism.rs::sockstat_consumers_are_deterministic_under_strict_verify
cargo test -p hermit --test softnet_stat_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/softnet_stat_determinism.rs
cargo test -p hermit --test softnet_stat_determinism softnet_stat_consumers_are_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/softnet_stat_determinism.rs::softnet_stat_consumers_are_deterministic_under_strict_verify
cargo test -p hermit --test sqlite_veryquick -- --include-ignored --test-threads=1 # [run] all tests in hermit-cli/tests/sqlite_veryquick.rs
cargo test -p hermit --test sqlite_veryquick sqlite_fast_subset_is_deterministic_under_hermit -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/sqlite_veryquick.rs::sqlite_fast_subset_is_deterministic_under_hermit
cargo test -p hermit --test sqlite_veryquick sqlite_veryquick_is_deterministic_under_strict_hermit -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/sqlite_veryquick.rs::sqlite_veryquick_is_deterministic_under_strict_hermit
cargo test -p hermit --test stress_suite -- --include-ignored --test-threads=1 # [run] all tests in hermit-cli/tests/stress_suite.rs
cargo test -p hermit --test stress_suite chaos_finds_and_reproduces_order_violation -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/stress_suite.rs::chaos_finds_and_reproduces_order_violation
cargo test -p hermit --test stress_suite targeted_chaos_finds_order_violation_at_least_as_often -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/stress_suite.rs::targeted_chaos_finds_order_violation_at_least_as_often
cargo test -p hermit --test stress_suite fast_chaos_matrix -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/stress_suite.rs::fast_chaos_matrix
cargo test -p hermit --test stress_suite slow_race_matrix -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/stress_suite.rs::slow_race_matrix
cargo test -p hermit --test stress_suite schedule_bisect_localizes_publish_ordering_race -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/stress_suite.rs::schedule_bisect_localizes_publish_ordering_race
cargo test -p hermit --test stress_suite slow_cas_search_and_replay -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/stress_suite.rs::slow_cas_search_and_replay
cargo test -p hermit --test swaps_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/swaps_determinism.rs
cargo test -p hermit --test swaps_determinism swaps_consumers_are_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/swaps_determinism.rs::swaps_consumers_are_deterministic_under_strict_verify
cargo test -p hermit --test syscall_file_io -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/syscall_file_io.rs
cargo test -p hermit --test syscall_file_io deterministic_file_io_syscalls_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/syscall_file_io.rs::deterministic_file_io_syscalls_verify
cargo test -p hermit --test syscall_file_metadata -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/syscall_file_metadata.rs
cargo test -p hermit --test syscall_file_metadata deterministic_file_metadata_syscalls_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/syscall_file_metadata.rs::deterministic_file_metadata_syscalls_verify
cargo test -p hermit --test syscall_quick_wins -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/syscall_quick_wins.rs
cargo test -p hermit --test syscall_quick_wins deterministic_passthrough_syscalls_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/syscall_quick_wins.rs::deterministic_passthrough_syscalls_verify
cargo test -p hermit --test sysfs_rtc_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/sysfs_rtc_determinism.rs
cargo test -p hermit --test sysfs_rtc_determinism sysfs_rtc_consumers_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/sysfs_rtc_determinism.rs::sysfs_rtc_consumers_verify
cargo test -p hermit --test sysv_legacy_fallbacks -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/sysv_legacy_fallbacks.rs
cargo test -p hermit --test sysv_legacy_fallbacks sysv_and_legacy_filesystem_features_fall_back_deterministically -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/sysv_legacy_fallbacks.rs::sysv_and_legacy_filesystem_features_fall_back_deterministically
cargo test -p hermit --test tcp_info_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/tcp_info_determinism.rs
cargo test -p hermit --test tcp_info_determinism tcp_info_hides_host_transport_counters_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/tcp_info_determinism.rs::tcp_info_hides_host_transport_counters_under_strict_verify
cargo test -p hermit --test thp_stats_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/thp_stats_determinism.rs
cargo test -p hermit --test thp_stats_determinism transparent_hugepage_stat_consumers_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/thp_stats_determinism.rs::transparent_hugepage_stat_consumers_verify
cargo test -p hermit --test thread_scheduling_fairness -- --include-ignored --test-threads=1 # [run] all tests in hermit-cli/tests/thread_scheduling_fairness.rs
cargo test -p hermit --test thread_scheduling_fairness four_runnable_threads_receive_round_robin_progress -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/thread_scheduling_fairness.rs::four_runnable_threads_receive_round_robin_progress
cargo test -p hermit --test thread_scheduling_fairness bounded_buffer_producer_and_consumers_complete -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/thread_scheduling_fairness.rs::bounded_buffer_producer_and_consumers_complete
cargo test -p hermit --test thread_scheduling_fairness rwlock_writer_is_not_starved_by_readers -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/thread_scheduling_fairness.rs::rwlock_writer_is_not_starved_by_readers
cargo test -p hermit --test thread_self_procfs_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/thread_self_procfs_determinism.rs
cargo test -p hermit --test thread_self_procfs_determinism thread_self_stat_consumers_are_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/thread_self_procfs_determinism.rs::thread_self_stat_consumers_are_deterministic_under_strict_verify
cargo test -p hermit --test thread_self_procfs_determinism thread_self_fd_keeps_the_opener_identity -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/thread_self_procfs_determinism.rs::thread_self_fd_keeps_the_opener_identity
cargo test -p hermit --test thread_sync_determinism -- --include-ignored --test-threads=1 # [run] all tests in hermit-cli/tests/thread_sync_determinism.rs
cargo test -p hermit --test thread_sync_determinism thread_sync_patterns_are_deterministic_across_five_runs -- --exact --include-ignored --test-threads=1 # [run] hermit-cli/tests/thread_sync_determinism.rs::thread_sync_patterns_are_deterministic_across_five_runs
cargo test -p hermit --test uevent_seqnum_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/uevent_seqnum_determinism.rs
cargo test -p hermit --test uevent_seqnum_determinism uevent_seqnum_consumers_are_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/uevent_seqnum_determinism.rs::uevent_seqnum_consumers_are_deterministic_under_strict_verify
cargo test -p hermit --test unix_socket_table_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/unix_socket_table_determinism.rs
cargo test -p hermit --test unix_socket_table_determinism unix_socket_table_consumers_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/unix_socket_table_determinism.rs::unix_socket_table_consumers_verify
cargo test -p hermit --test vmstat_determinism -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/vmstat_determinism.rs
cargo test -p hermit --test vmstat_determinism vmstat_consumers_are_deterministic_under_strict_verify -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/vmstat_determinism.rs::vmstat_consumers_are_deterministic_under_strict_verify
cargo test -p hermit --test writev_determinism -- --include-ignored --test-threads=1 # [both/mixed] all tests in hermit-cli/tests/writev_determinism.rs
cargo test -p hermit --test writev_determinism writev_uses_fd_aware_scheduling_and_verifies -- --exact --include-ignored --test-threads=1 # [both/mixed] hermit-cli/tests/writev_determinism.rs::writev_uses_fd_aware_scheduling_and_verifies
cargo test -p hermit --test zero_copy_pipe_fallback -- --include-ignored --test-threads=1 # [verify] all tests in hermit-cli/tests/zero_copy_pipe_fallback.rs
cargo test -p hermit --test zero_copy_pipe_fallback zero_copy_pipe_syscalls_fall_back_only_in_strict_mode -- --exact --include-ignored --test-threads=1 # [verify] hermit-cli/tests/zero_copy_pipe_fallback.rs::zero_copy_pipe_syscalls_fall_back_only_in_strict_mode
cargo test -p hermit --lib --bins -- --test-threads=1 # [both/mixed] ci/dag/hosted.json test/hermit_unit
cargo test -p hermit --test aio_nr_determinism --test arch_status_determinism --test chaos_sched_yield_progress --test chaos_stress_pmu_detection --test clock_determinism --test clock_discipline_determinism --test cpufreq_avg_determinism --test epoll_determinism --test file_nr_determinism --test fp_reduction_determinism --test futex2_refusal --test hashseed_determinism --test inode_nr_determinism --test kernel_keyring --test key_users_determinism --test mmap_determinism --test node_vmstat_determinism --test numa_maps_determinism --test perf_event_refusal --test pidfd_creation --test process_isolation_refusals --test proc_fdinfo_determinism --test proc_locks_determinism --test procfs_determinism --test procfs_positioned_determinism --test pty_nr_determinism --test python_stdlib --test self_sched_determinism --test self_schedstat_determinism --test signal_determinism --test smaps_determinism --test smaps_rollup_determinism --test softnet_stat_determinism --test sockstat_determinism --test swaps_determinism --test thp_stats_determinism --test zero_copy_pipe_fallback -- --test-threads=1 # [both/mixed] ci/dag/hosted.json test/hermit_integration
cargo test -p hermit --test arbitrary_binaries -- --skip record_replay_stable_arbitrary_binaries --test-threads=1 # [run] ci/dag/hosted.json test/arbitrary_binaries
cargo test -p hermit --test cli -- --skip run_kvm_ --skip backend_accepted_in_global_position --skip run_dbi_aggregates_unsupported_syscalls_and_strict_rejects_them --skip run_dbi_strict_returns_with_blocked_stdin_source --skip run_dbi_verifies_pipe_backpressure --skip run_dbi_keeps_diagnostics_out_of_guest_stderr --skip run_dbi_recovers_after_failed_exec --skip run_liteinst_rejects_non_fork_clone --skip run_liteinst_handles_inherited_ignored_sigchld --skip run_liteinst_verifies_forked_guest --skip run_liteinst_verifies_raw_fork_guest --test-threads=1 # [both/mixed] ci/dag/hosted.json test/cli
cargo test -p hermit --test hermit_modes -- --skip default_ --skip chaos_buck_ --skip hello_race_chaos_verify --test-threads=1 # [both/mixed] ci/dag/hosted.json test/hermit_modes
cargo test -p hermit --test app_strict_verify -- --ignored --skip java_ --skip javac_ --test-threads=1 # [verify] ci/dag/hosted.json test/app_strict_verify
cargo test -p hermit --test command_strict_verify -- --ignored --test-threads=1 # [verify] ci/dag/hosted.json test/command_strict_verify
cargo test -p hermit --test epoll_determinism --test rcx_canonicalization -- --ignored --test-threads=1 # [both/mixed] ci/dag/hosted.json test/ignored_syscall_regressions
cargo test -p hermit --test rr_suite rr_scratch_directories_are_fresh_and_cleaned -- --exact # [run] ci/dag/hosted.json test/rr_suite_contract
python3 tests/backend-parity/run_matrix.py --hermit target/release/hermit --backend dbi --require-backend # [both/mixed] ci/dag/hosted.json test/dbi_parity
set -e; HERMIT=target/debug/hermit; ARGS='run --base-env=minimal --no-virtualize-cpuid --max-timeslice=disabled'; REPS=${L4_REPS:-20}; run_probe() { local c="$1"; timeout 30s $HERMIT $ARGS --strict -- $c </dev/null; timeout 30s $HERMIT $ARGS --strict --verify -- $c </dev/null; timeout 30s $HERMIT $ARGS --strict --verify --detlog-heap --detlog-stack -- $c </dev/null; local i; for ((i=0;i<REPS;i++)); do timeout 30s $HERMIT $ARGS --strict --verify -- $c </dev/null; done; }; run_probe '/bin/true'; run_probe '/bin/echo hermit-envelope'; run_probe '/bin/date -u +%Y' # [verify] ci/dag/hosted.json test/envelope_levels
./scripts/validate.rs --hosted-strict-compat-only --no-label-pr --verbose # [verify] ci/dag/hosted.json test/strict_compat
cargo test -p hermit --test cli run_kvm_ -- --test-threads=1 # [both/mixed] ci/dag/hardware.json kvm/cli
cargo test -p hermit --test cli backend_accepted_in_global_position -- --exact --test-threads=1 # [both/mixed] ci/dag/hardware.json kvm/global_position
cargo test -p hermit --test arch_prctl --test compression --test madvise --test ppoll_simulation --test redis_strict --test sqlite_veryquick --test syscall_file_io --test syscall_file_metadata --test syscall_quick_wins --test thread_scheduling_fairness --test writev_determinism -- --test-threads=1 # [both/mixed] ci/dag/hardware.json hw/integration
cargo test -p hermit --test record_replay -- --skip record_replay_matrix --test-threads=1 # [record/replay] ci/dag/hardware.json rr/stable
cargo test -p hermit --test arbitrary_binaries record_replay_stable_arbitrary_binaries -- --exact --test-threads=1 # [record/replay] ci/dag/hardware.json rr/arbitrary
cargo test -p hermit --test random_determinism random_sources_are_deterministic_under_strict_verify -- --exact --ignored --test-threads=1 # [verify] ci/dag/hardware.json random/strict_verify
cargo test -p hermit --test analyze -- --ignored --skip analyze_hello_race --test-threads=1 # [both/mixed] ci/dag/hardware.json analyze/pmu
cargo test -p hermit --test language_runtime_determinism -- --ignored --test-threads=1 # [both/mixed] ci/dag/hardware.json runtime/entropy
cargo test -p hermit --test python_stdlib -- --ignored --test-threads=1 # [both/mixed] ci/dag/hardware.json python/stdlib
cargo test -p hermit --test stress_suite slow_cas_search_and_replay -- --exact --ignored --test-threads=1 # [both/mixed] ci/dag/hardware.json stress/search_replay
./hermit-cli/tests/prepare_leveldb.sh target/hermit-leveldb-ci target/hermit-leveldb-build-ci # [run] ci/dag/hardware.json leveldb/build_fixture
env HERMIT_LEVELDB_BUILD_DIR=target/hermit-leveldb-build-ci cargo test -p hermit --test leveldb focused_leveldb_tests_are_deterministic_under_strict -- --exact --test-threads=1 # [both/mixed] ci/dag/hardware.json leveldb/focused
env HERMIT_LEVELDB_BUILD_DIR=target/hermit-leveldb-build-ci cargo test -p hermit --test leveldb leveldb_env_posix_is_deterministic_under_strict -- --exact --ignored --test-threads=1 # [both/mixed] ci/dag/hardware.json leveldb/env_posix
cargo test -p hermit --test redis_strict -- --ignored --test-threads=1 # [both/mixed] ci/dag/hardware.json redis/extended
test -f third-party/rr/src/test/util.h || { echo 'FAIL: PMU rr syscall suite requires initialized third-party/rr' >&2; exit 1; }; cargo test -p hermit --test rr_suite -- --ignored --skip rr_ppoll --skip rr_rlimit --skip rr_sched_yield_to_lower_priority --test-threads=1 # [record/replay] ci/dag/hardware.json rr/suite
set -e; HERMIT=target/debug/hermit; for c in '/bin/true' '/bin/echo hermit-envelope' '/bin/date -u +%Y'; do timeout ${HERMIT_RR_TIMEOUT:-30s} $HERMIT record start --verify -- $c </dev/null; done # [record/replay] ci/dag/hardware.json rr/envelope
./scripts/validate.rs --rr-compat-only --no-label-pr # [record/replay] ci/dag/hardware.json rr/compat_baseline
./tests/debugger/run_debugger_tests.sh # [both/mixed] ci/dag/hardware.json debugger/integration
python3 tests/backend-parity/run_matrix.py --backend ptrace # [both/mixed] ci/dag/hardware.json ptrace/parity
