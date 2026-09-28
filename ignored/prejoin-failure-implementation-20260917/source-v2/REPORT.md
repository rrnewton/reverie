# Reverie pre-join failure source-v2

Source-v2 preserves source-v1 and makes the two corrections requested after root read the original complete report and caller. It remains uncommitted. The full source-v1 scope and explicit missing Hermit integration coverage still apply; this report records only the correction and actual attempted validation.

Base `114b309413612fafc2657c74e83811c71aac7b19`. Binding `fba65b3fa1797f33a7598687a245603727b0e03ee0fcd03f9c77d38bf77dbbc4`. Complete 102196-byte patch `a3dc4d84bfc049daa644a1f54e958b2879b67654b8c31d6e78d527e7f76cea15`.

Worker failures now carry their TID in a typed `Error::WorkerFailure` wrapper. Display retains `KVM worker cleanup failed: thread <tid>:` and `primary()` and the error source chain retain the original typed cause. The new RPC controls additionally require the thread 2 diagnostic and a typed GuestClock primary. No existing assertion is relaxed.

`RpcControlCleanup` owns the response rescue and group join before the first OS thread is spawned or an ordering precondition can fail. During unwind it sends the rescue response and joins the actual owned OS thread, through either the already running joiner or production `GuestThreadGroup::join_workers`. The original precondition assertion remains a failure; a rescue cannot become a passing RPC response. The previous 2 second deadline is unchanged.

The corrected execution plan names `/home/newton/work/dev-hermit/worktrees/slots/kvm-proc-fd-identity-20260917/ignored/prejoin-failure-implementation-20260917/cargo-v2/launch.py`. The caller also checks its own path against that field. Caller SHA256 `2dbb36eee6d034cc9b50746669bd7dd99a4a2c914dbba1ba078bf27ae843c9b0`; plan SHA256 `b657a9ed68171522c25e02b727523d7aa03fbf57ddc361104adc58d52b4d2b2e`. Source-v1 and its incorrect but unexecuted plan remain intact.

Root released this corrected candidate for one bounded compile/list/native sequence only, with no source or landing approval. The selected population stayed exactly 27: 8 new native controls and 19 nearby existing controls. The unchanged observer SHA256 is `f10ab861f262dbbd18295d92e59e05174299b72b397f58de844ee1725266eae6`. Fresh outputs use `measurement-prejoin-native-20260917/cargo-v2/{compile,list,native}` and a fresh `target/prejoin-native-v2`. Bounds remained compile 600 CPU/900 wall seconds, list 5/15, native 30/60, memory 16 GiB, zero swap and 2 jobs.

The actual attempt refused at Cargo dependency resolution before any compilation. Cargo exited 101 because the existing separately bound Cargo.lock needs updating and `--locked` forbids it. The original 67056-byte lockfile SHA256 remains `d432b018c022722ac6a8d121a652402dd6089f0eb92e5ab6bdbc489992e00469`.

The refusal consumed 0.157321 aggregate CPU seconds and 0.816144485 wall seconds. Accounting is complete. The service is empty and inactive in the observer and in a fresh independent systemctl read. There was no observer error, elapsed bound, output truncation, inventory, native test, guest, or Hermit run. List and native output directories do not exist.

Retained records:

- `../cargo-v2/FAILURE.json`: concise cause and final-accounting record.
- `../cargo-v2/run-1/launch.json`, SHA256 `dab0f60b20f359540417bc0db01cf347a05e413faa85ee27e077cfe1cfb60b92`: actual corrected caller, environment and source identities.
- Observer compile `result.json`, SHA256 `df7e7d90c6b886531b650eb3b56f038e1736de54a2a7d9a9216894475301063d`: full bounded-run evidence.
- Observer compile stderr, SHA256 `9d3d50110f7faf9fdc1e3a69ae9bb0643168d44cbd681af15ef8fae3c044eaad`: complete 280-byte Cargo refusal.

A read-only postfailure check verified all frozen source, lockfile and observer input identities unchanged. No changed follow-up has run. The root coordinator received the concrete refusal before any proposed dependency-lock repair. Native controls remain unexecuted and cannot qualify guest parity or the real Detcore registration/selection/setup cleanup paths.

## Subsequent accepted lock and native result

Root accepted the exact existing-manifest lock refresh and released a new bound compile/list/27-native plan. Source-v2 remained unchanged. The cargo-v4 sequence compiled and passed all 27 selected tests, with no failed or ignored cases, complete accounting and empty services. Detailed current evidence is `../cargo-v4/REPORT.md` and `../cargo-v4/RESULT.json`. The original refusal above remains preserved as a pre-compilation refusal. No Hermit or guest run occurred, and real scheduler/registered-child integration coverage remains outstanding.
