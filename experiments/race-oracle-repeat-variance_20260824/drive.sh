set -u
cd /home/newton/work/dev-hermit
B=worktrees/005/pin/target/release/hermit
R=20
for i in $(seq 1 $R); do
  for m in concurrent sequential; do
    id=$(printf "%s-%02d" "$m" "$i")
    {
      echo "REP=$id"
      echo "PRE_LOAD=$(awk '{print $1}' /proc/loadavg)"
      echo "PRE_HERMIT=$(pgrep -c hermit 2>/dev/null || echo 0)"
      echo "PRE_QEMU=$(pgrep -c qemu 2>/dev/null || echo 0)"
    } >> /tmp/h005-oracle/census.txt
    timeout 1200 ./bin/hermit-repeat -n 20 --mode "$m" --backend sabre \
      --sh-bin "$B" --json /tmp/h005-oracle/reps/$id.json \
      --out /tmp/h005-oracle/reps/$id.out \
      -- /bin/sh -c 'seq 1 2000 | wc -l' > /tmp/h005-oracle/reps/$id.log 2>&1
    echo "EXIT=$?" >> /tmp/h005-oracle/census.txt
    {
      echo "POST_LOAD=$(awk '{print $1}' /proc/loadavg)"
      echo "POST_HERMIT=$(pgrep -c hermit 2>/dev/null || echo 0)"
      echo "POST_QEMU=$(pgrep -c qemu 2>/dev/null || echo 0)"
      echo "--"
    } >> /tmp/h005-oracle/census.txt
    rm -rf /tmp/h005-oracle/reps/$id.out
  done
  echo "done rep $i" >> /tmp/h005-oracle/progress.txt
done
echo ALLDONE >> /tmp/h005-oracle/progress.txt
