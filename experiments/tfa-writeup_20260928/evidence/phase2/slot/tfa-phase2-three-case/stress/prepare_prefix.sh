#!/bin/bash
# Controller-owned stress checkouts: clone at the manifest revision and build the
# unfixed test ELF with the manifest build_argv. No investigator writes here.
set -u
E3=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case
export TMPDIR=$E3/tmpdir
REV=7f239d5b37dfbecf32fed144468da461f998add5
L=$E3/stress/logs
mkdir -p /home/newton/work/tfa/phase2-cases/stress
declare -A TARGET=(
  [thrift]="@fbcode//mode/dev-tsan fbcode//thrift/lib/cpp2/test:InstrumentationTest"
  [asyncssl]="@fbcode//mode/libcxx fbcode//folly/io/async/test:async_ssl_socket_test"
  [httpcoro]="@fbcode//mode/dev-asan fbcode//proxygen/lib/http/coro/test:HTTPCoroSessionTests"
)
for c in "$@"; do
  D=/home/newton/work/tfa/phase2-cases/stress/$c
  date -u +%FT%TZ > $L/clone-$c.started
  sl clone --eden fb:fbsource $D --reason "create controller-owned TFA $c stress checkout - sl help clone" > $L/clone-$c.out 2>&1 \
    && (cd $D && sl goto $REV --reason "check out TFA phase-two manifest source revision - sl help goto") >> $L/clone-$c.out 2>&1
  echo $? > $L/clone-$c.rc
  (cd $D && sl status --reason "record stress checkout cleanliness before build - sl help status") > $L/status-prebuild-$c.out 2>&1
  date -u +%FT%TZ > $L/build-prefix-$c.started
  (cd $D && buck build ${TARGET[$c]} --show-full-json-output) > $L/build-prefix-$c.out 2> $L/build-prefix-$c.err
  echo $? > $L/build-prefix-$c.rc
  date -u +%FT%TZ > $L/build-prefix-$c.finished
done
touch $L/prepare.DONE
