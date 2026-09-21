# LogStore Health native concurrency profile

This diagnostic profile does not alter the precommitted cell, oracle, or seed
population. It explains why the cell is stopped after its censored seed 0.

The exact selected case `LogStoreHealthMonitorTest.DisabledRebuilding` showed
genuine schedule sensitivity:

- an ordinary bounded native run reached a worker-thread `SIGSEGV` in
  `LogStoreHealthMonitor::executeDueTriggers` after 4.56 seconds;
- a bounded `strace` run passed the same selected case in 2.673 seconds;
- that passing trace recorded 657 thread IDs, 656 `clone3` calls, 16,739 futex
  calls (4,753 waits and 11,986 wakes), 493 eventfds, 475 epoll instances, and
  4,050 epoll waits.

The raw diagnostics are retained under
`/home/newton/work/dev-hermit/ignored/tfa-kernel71-campaign/screen-logstore-health`.
This is a real multithreaded flake, but its thread fanout makes it unsuitable
for the present ptrace campaign. Formal fuzz seed 0 already ran for 38 seconds
and was killed at the 64 MiB stderr cap before the selected-test marker, so it
is censored and no further seeds are authorized by current campaign direction.
