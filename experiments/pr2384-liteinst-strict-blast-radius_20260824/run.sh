set -x
L=/tmp/h005-lite
NODE='HERMIT_LITEINST_TEST_BINARY=$PWD/target/release/hermit ./ci/run-nextest-counted.sh -p hermit --features third-party-backends --test liteinst_advanced -j 1'
for side in base:/home/newton/work/dev-hermit/worktrees/005/depfix head:/home/newton/work/dev-hermit/worktrees/005/p2384; do
  tag=${side%%:*}; dir=${side#*:}
  cd "$dir"
  cargo build --release --locked -p hermit --features third-party-backends > $L/$tag-build.log 2>&1
  echo "${tag}_BUILD_EXIT=$?" >> $L/exits
  ./scripts/stage-liteinst-runtime.sh release "$PWD/target/release/libreverie_liteinst.so" "$PWD/target/liteinst-runtime-build" > $L/$tag-stage.log 2>&1
  echo "${tag}_STAGE_EXIT=$?" >> $L/exits
  HERMIT_LITEINST_TEST_BINARY=$PWD/target/release/hermit ./ci/run-nextest-counted.sh -p hermit --features third-party-backends --test liteinst_advanced -j 1 > $L/$tag-test.log 2>&1
  echo "${tag}_TEST_EXIT=$?" >> $L/exits
done
echo DONE >> $L/exits
