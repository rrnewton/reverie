## Offline Benchmark Environment (identical for every run)
This investigation runs OFFLINE inside the Crash Cortex benchmark harness. The facts below override any
tool or path that the /kernel-investigation skill mentions.

- **No fleet, Scuba, ODS, OOBit, serf, SSH, vmcore, Workplace, Phabricator or internet access.** kernel-mcp,
  ssh-session and ODS MCP tools do not exist here. Everything known about the crash is in `{LOG_DIR}/`.
  Do not try to reach the network; network commands are denied and any attempt is logged.
- **Kernel source:** your working directory `{LINUX}` is a git worktree checked out at the exact crash-kernel
  commit (crash kernel `{CRASH_KERNEL}`). Its history (branches `v*-fb`, upstream `linus-upstream`/`master`,
  tags) is cut at the benchmark cutoff (at or before the crash time): later commits are not available. There is no git remote.
  Use `git log`, `git blame`, `git show`, `git log -S/-G` etc. on this tree for code archaeology and fix search.
- **The /kernel-investigation skill** is at `{SKILL_DIR}/SKILL.md`; its subagent files are in `{SKILL_DIR}/subagents/`.
  Read the SKILL.md first and follow it. Treat every kernel-mcp step (session_create, mkosi_build,
  mkosi_start_qemu, git_fetch, b4_fetch, query_*) as unavailable and use the local equivalents below.
- **Reproducer / A-B toolchain (replaces mkosi-kernel):** gcc, make, flex, bison, bc, pahole, cpio are installed;
  build a guest kernel from `{LINUX}` with `make O=<dir> ...` (e.g. `defconfig` + `kvm_guest.config` + the options your
  reproducer needs) and boot it with `qemu-system-x86_64` (QEMU 8.1.2, on PATH from `{QEMU_DIR}`, `-enable-kvm`
  works) using `-kernel <bzImage> -initrd <your cpio> -nographic -append "console=ttyS0 ..."`. Guest networking is
  disabled. Always bound runs with `timeout`. Keep build dirs and reproducer artifacts under `{LOG_DIR}/reproducer/`
  (build trees may go under `{LOG_DIR}/build/`); both are collected after the run.
- **Duplicate check (Phase 7):** there are no other reports in this environment; set `DUPLICATE_OF: unchecked`.
- **Diff submission:** do not run `jf`/`arc`; just leave `kernel_fix.patch` in `{LOG_DIR}/`.
- **Budget:** this run is capped at 60 minutes wall time and 40M tokens; the harness stops the run at either cap
  and grades whatever is in `{LOG_DIR}/report.md` at that moment, so write report.md early and update it.
