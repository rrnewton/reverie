# Hermit website overhaul — changes v2

Implemented Ryan's approved plan and follow-up notes on 2026-10-01.

- **Site branch:** [writer/landing-page-v1](https://github.com/rrnewton/hermit/tree/writer/landing-page-v1)
- **Site commit:** [`e212df558952f17818832227d1ac6b119f6576a1`](https://github.com/rrnewton/hermit/commit/e212df558952f17818832227d1ac6b119f6576a1), based on `342433e251d75624d49eaacaa111a583e07c983f`.
- **Changed paths:** `docs/site/index.html`, `docs/site/site.css`, and the introductory blog citation in `README.md`.
- **Handoff:** branch pushed and all three files read back byte-for-byte from GitHub. Coord2 owns PR creation, review, validation, landing, and deployment. This report does not claim the new page is live.

## How Ryan's notes were applied

| Note | Implementation |
| --- | --- |
| Keep “Hermetic Infra” as branding | Retained in both header and footer, and in the browser title. The hero introduces Hermit. |
| Say “program,” including builds and CI | Hero says “a program's outcome.” Why Hermit starts with “A running program, such as a build job or a CI test, has more inputs…” |
| Drop the blog byline | Prominent hero link uses the article title without an author credit. README citation also omits the byline. |
| Explain control of external services | Used the requested wording: “Responses from external services outside the container must also be controlled: repeatable, blocked, or recorded.” |
| Beta record/replay, rr, debugger, DAP | Added a Beta panel beside the getting-started material, linked Mozilla's rr, and linked the public record/replay and DAP instructions. Names GDB and expands Debug Adapter Protocol. Retains the narrower workload coverage qualification. |
| Qualitative compatibility highlights | Added Python, Node.js, Rust, and SQLite workload descriptions, each linked to a published passing scorecard cell. Kept a prominent latest-scorecard link; no manually maintained counts or chart. |
| Keep high-water examples | Added cards for Linux boot/drgn inspection, a btrfs-convert concurrency bug, and Debian/Nix build experiments. Exact evidence limits are recorded below. |
| Keep the animation | Guest / Hermit / Linux stay stacked in the center. Six entropy labels orbit them and remain upright. A keyboard-accessible checkbox pauses motion. Reduced-motion mode is static; narrow screens use the same stack and a label grid. |
| Public website only | Removed private dev-hermit links. New page links are limited to the public Hermit/Reverie repositories, rr, the original blog, API docs, and the public scorecard. LiteInst's link goes to its public Hermit integration documentation. Nix is described without a link. |

## Evidence behind the added claims

### Compatibility

All four highlighted cells have published **ptrace / verify / Pass** results dated **2026-09-30T00:01:26Z**, at Hermit `84da7b816939fcce1500ec8d933769d3dc993d14`. They compare repeated executions; they are not record/replay results or cross-backend comparisons.

| Highlight | Published result and scope |
| --- | --- |
| Python | [Dictionary/set workload](https://rrnewton.github.io/hermit/compatibility/latest/cells/detail.html?cell=29686202d784eee06e035afd32191edffee25c2946b9a01b11ee184d28f506b1): hash collections, file I/O, and a sort subprocess. |
| Node.js | [JavaScript workload](https://rrnewton.github.io/hermit/compatibility/latest/cells/detail.html?cell=494000d5da5ced70055b6b51fa3eb54e5ab9ab6f974d79acb6aa92083eab9880): hot numeric loop and random numbers. It does not independently assert a particular JIT optimization tier. |
| Rust | [HashMap/HashSet workload](https://rrnewton.github.io/hermit/compatibility/latest/cells/detail.html?cell=e94153044ab653736c80d6451d14d31217a89201347a0780fbf0d45fe11487d7): randomized collections, files, and SystemTime. |
| SQLite | [Database workload](https://rrnewton.github.io/hermit/compatibility/latest/cells/detail.html?cell=841666ca0a4d9bc178f3bd642449fa8555cee6159e19ddfe52d8d063d4b5e613): create/reopen a database, aggregates, ordered rows, random values, and time. |

The complete published catalogue inspected did not contain Java/JVM, Go, GHC, or PostgreSQL test IDs. Those names are omitted. Local manifests and fixtures support the workload descriptions: `tests/e2e/manifests/language-runtimes.yaml`, `data-handling.yaml`, and their adjacent workload directories. The requested `ci/compat/compat.yaml` is absent; its current counterpart is `tests/e2e/manifests/compat.yaml`. Test selection in that manifest is not itself a passing result.

### Record/replay

Read the [September record-mode assessment](../hermit-record-mode-current-state-20260915.md), current CLI and `hermit-dap` source, and the public [rr introduction](https://rr-project.org/). The site uses **Beta**, as Ryan requested. The rr comparison describes the record/debug workflow; it does not promise rr-equivalent reverse-debugging performance. Hermit's current DAP reverse stepping restarts replay and runs forward.

### Larger examples

- **QEMU/Linux:** [the original strict-boot experiment](../../experiments/qemu_strict_l2_boot_20260727/README.md) reported stronger verification than the [current public QEMU guide](https://github.com/rrnewton/hermit/blob/main/docs/QEMU_BOOT.md) supports. That guide corrects the result to comparison after numeric/address/path/time normalization. The card therefore says **Boot and inspect Linux under QEMU**, with controlled scheduling and normalized repeat-boot checks, rather than claiming fully deterministic Linux execution. [drgn execution-control experiments](../../experiments/drgn-exec-control_20260818/README.md) establish repeatable task-list snapshots at chosen stops, not full-run memory identity. Also consulted the `qemu-linux` and `qemu-coherent-timebase_20260818` experiment documentation.
- **btrfs:** [the successful experiment](../../experiments/btrfs-convert-progress-uaf-chaos_20260729/README.md) restored a historical userspace use-after-free in btrfs-convert. Buggy and fixed variants shared an adaptation that made the racing progress-thread wakeup observable, plus AddressSanitizer instrumentation. Chaos seeds triggered the buggy crash, and repeating a seed reproduced the guest report. The fixed version had no such crashes in the same sweep, although some schedules timed out in both variants. The separate [kernel subvolume-race experiment](../../experiments/btrfs_f6a6c280_subvol_race_repro_20260729/README.md) did not reproduce its target; [progress-output interleavings](../../experiments/btrfs_seeds_to_reproduce_benchmark_20260728/README.md) alone were benign. The site claims neither of those as a reproduced bug.
- **Builds:** [Debian pilot results](../../experiments/debian_reproducible_builds_2026/PILOT_RESULTS.md) record byte-identical hello, hostname, and tree package files across independent build roots. [The Nix builder prototype](../../experiments/nix-hermit-execbuilder-prototype_20260729/README.md) made controlled clock/random-byte examples reproducible. The [later host-Nix investigation](../../experiments/nix_hermit_repro_hostnix_20260806/README.md) explicitly does not establish previously irreproducible real nixpkgs packages becoming reproducible. The card describes example builds and a prototype. Also read the container-approach and minimum-Hermit-dose experiments; their older configuration claims are not repeated.

## Verification and remaining handoff

**Works? YES** — opened `docs/site/index.html` in headless Chromium through Playwright. The page and stylesheet loaded; Get started scrolled to the command example. Pause worked by click and keyboard, and reduced-motion mode stopped animation while retaining the labels.

**UX sensible? YES** — inspected full-page desktop (1440 px) and mobile (390 px) screenshots. The page explains Hermit before components or compatibility, uses readable public links, and keeps the central stack and labels legible. No unresolved rendering issue was found.

- No horizontal page overflow at widths 320, 390, 600, 768, 1024, 1101, 1280, and 1440 px. On every animated layout in that set, labels stayed clear of the central stack at 24 sampled positions around a full orbit.
- All 15 distinct linked public URL targets returned HTTP 200 using `with-proxy curl -sI -L`. Checked local document headings for the linked GitHub fragments; checked every in-page fragment against the HTML IDs. Scorecard data was fetched separately to verify that its linked cells actually contain the claimed passing results.
- HTML tag nesting, unique IDs, accessibility references, the public URL allowlist, and `git diff --check` passed. No browser page errors.
- Product tests, assertions, tolerances, comparators, classifications, and validation gates are unchanged. The removed landing-page counters were static presentation, not test gates. No Hermit program run or product validation was performed for this website change; coord2 owns the scheduled validation and review.
- Shared local review artifacts are under `ignored/website-overhaul/writer-e212df558/`: `writer-site-desktop.png`, `writer-site-mobile.png`, `writer-site-browser-check.py`, `writer-site-browser-check.txt`, and `writer-site-link-check.json`. These are ignored review aids, not website assets.

**Open question for Ryan:** should the Nix builder prototype and a short reproducible-builds report be published in `rrnewton/hermit`, so the landing page can link to them? The supplied prototype currently has no public home. A public btrfs experiment write-up would also let that card link directly to its evidence; its present link is explicitly labeled as the general schedule-exploration guide.

## Final page copy

Header brand: **Hermetic Infra**. Navigation: **Why Hermit? · Software components · Compatibility · Examples**.

The following copy is transcribed from the committed HTML. Links within the page are shown with their eventual public destinations.

Hermit

### Tools for Reproducible Linux Execution

Hermit runs unmodified Linux programs in a controlled, repeatable environment. It controls inputs that can change a program's outcome: thread scheduling, time, and random numbers. Use it to reproduce failures, explore different thread schedules, and investigate concurrency bugs.

[Get started](https://rrnewton.github.io/hermit/#try-hermit) · [Rust API](https://rrnewton.github.io/hermit/hermit/index.html)

Read the original introduction: [Hermit: Deterministic Linux for Controlled Testing and Software Bug-finding](https://developers.facebook.com/blog/post/2022/11/22/hermit-deterministic-linux-testing/)

**Diagram:** Guest — Your Linux program / Hermit — Controls execution / Linux — Host kernel.

**Orbit labels:** Thread scheduling, Clocks, Random bytes, Process IDs, File metadata, CPU clock and features.

**Control:** Pause animation.

*Caption:* Hermit sits between your program and Linux, controlling system calls and processor events that can make executions differ.

### Why Hermit?

A running program, such as a build job or a CI test, has more inputs than its arguments and files. Its result can also depend on which thread runs first, what the clock says, or which random bytes it receives. These implicit inputs can make a failure difficult to reproduce.

#### Repeat an execution

Run a supported program again with the same inputs and settings to investigate a failure.

#### Look for concurrency bugs

Change the scheduling seed to explore other thread interleavings. Keep the seed to reproduce an interesting run with the same program and Hermit build.

#### Narrow down a failure

Compare passing and failing schedules to find changes in event order that trigger the bug.

Reproducible runs need a fixed program, input files, environment, and Hermit configuration, including the starting clock time. Responses from external services outside the container must also be controlled: repeatable, blocked, or recorded.

### Try Hermit

[Install Hermit on x86-64 Linux](https://github.com/rrnewton/hermit#install-from-source), then try reading random bytes:

*In your terminal*

```sh
hermit run -- /bin/sh -c 'od -An -N8 -tx1 /dev/urandom'
```

Run the command twice. With the same Hermit build and random seed, it prints the same bytes. Hermit supplies repeatable data when the program asks Linux for randomness.

To run your own program, place `hermit run --` before its command. The [user guide](https://github.com/rrnewton/hermit/blob/main/docs/USER_GUIDE.md) explains host requirements, clock settings, and schedule exploration.

Beta

#### Record and replay

Record a program execution, then replay it to investigate a failure. Hermit offers a workflow similar to [Mozilla's rr](https://rr-project.org/), with GDB debugging and Debug Adapter Protocol (DAP) support for editors and other debugging tools.

Record/replay supports fewer workloads than deterministic execution.

[Record/replay guide](https://github.com/rrnewton/hermit/blob/main/docs/USER_GUIDE.md#record-and-replay) · [DAP instructions](https://github.com/rrnewton/hermit#debug-adapter-protocol)

### Software components

Use Hermit to run programs, or use the libraries below to build your own tools.

Run programs

#### Hermit

A command-line tool for repeatable execution and concurrency testing of x86-64 Linux programs. Hermit uses Reverie to intercept system calls and processor events, then controls scheduling, time, and other inputs to the program.

[Source](https://github.com/rrnewton/hermit) · [Rust API](https://rrnewton.github.io/hermit/hermit/index.html)

Build tools

#### Reverie

A Rust library for building tools that observe and change the behavior of Linux programs. Write handlers for system calls and processor events to inspect a program, change results, or perform additional operations. Hermit uses these handlers to implement deterministic execution.

[Source and guide](https://github.com/rrnewton/reverie)

Patch instructions

#### LiteInst

A library for patching x86-64 machine instructions while a program runs. LiteInst2 brings the original C++ project's patching core to Rust. Hermit's experimental LiteInst integration uses this technique to intercept program operations.

[Hermit integration](https://github.com/rrnewton/hermit#execution-backends)

### Compatibility

Hermit supports x86-64 Linux. Which programs work depends on the system calls they use, the host machine, and how Hermit intercepts their execution.

These interception mechanisms are called execution backends. Start with the default, **ptrace**, which uses Linux's process-tracing interface and is the most thoroughly tested. Other backends are experimental or specialized.

Recorded passing workloads on the ptrace backend include:

- [**Python**](https://rrnewton.github.io/hermit/compatibility/latest/cells/detail.html?cell=29686202d784eee06e035afd32191edffee25c2946b9a01b11ee184d28f506b1) scripts using hash collections, files, and subprocesses.
- [**Node.js**](https://rrnewton.github.io/hermit/compatibility/latest/cells/detail.html?cell=494000d5da5ced70055b6b51fa3eb54e5ab9ab6f974d79acb6aa92083eab9880) running a hot JavaScript loop and generating random numbers.
- [**Rust**](https://rrnewton.github.io/hermit/compatibility/latest/cells/detail.html?cell=e94153044ab653736c80d6451d14d31217a89201347a0780fbf0d45fe11487d7) programs using randomized hash collections, files, and clocks.
- [**SQLite**](https://rrnewton.github.io/hermit/compatibility/latest/cells/detail.html?cell=841666ca0a4d9bc178f3bd642449fa8555cee6159e19ddfe52d8d063d4b5e613) creating and reopening a database, then running queries that use random numbers and time.

These checks compare repeated executions. The scorecard records results separately for each workload, mode, and backend; try the workload you depend on.

[Latest compatibility scorecard](https://rrnewton.github.io/hermit/compatibility/latest/)

Experiments

### Linux, filesystems, and builds

Some of the larger workloads explored with Hermit.

#### Boot and inspect Linux under QEMU

A minimal Linux guest boots under QEMU with Hermit's controlled thread scheduling and QEMU's instruction-based clock. Repeated boots matched after log normalization. Separate drgn experiments reproduced kernel task lists at chosen stopping points.

[QEMU boot guide](https://github.com/rrnewton/hermit/blob/main/docs/QEMU_BOOT.md)

#### Reproduce a btrfs concurrency bug

With a historical bug restored in an instrumented btrfs-convert, Hermit's chaos scheduler triggered a use-after-free. Repeating the seed reproduced the same failure report; the fixed version had no such crashes in the same sweep.

[Explore thread schedules](https://github.com/rrnewton/hermit/blob/main/docs/USER_GUIDE.md#chaos-mode)

#### Reproduce build outputs

Hermit has rebuilt Debian packages such as hello, hostname, and tree with byte-identical package files across independent build environments. A Nix builder prototype also made example builds that read the clock and random bytes reproducible.

Footer brand: **Hermetic Infra**. Links: [Source](https://github.com/rrnewton/hermit) · [Documentation](https://github.com/rrnewton/hermit#more-documentation) · [Examples](https://github.com/rrnewton/hermit/tree/main/examples) · [Report an issue](https://github.com/rrnewton/hermit/issues) · [Page source](https://github.com/rrnewton/hermit/tree/main/docs/site).

## README addition

Immediately before **Requirements**:

For an introduction to the ideas behind Hermit, read [Hermit: Deterministic Linux for Controlled Testing and Software Bug-finding](https://developers.facebook.com/blog/post/2022/11/22/hermit-deterministic-linux-testing/). It explains how controlling thread schedules and other implicit inputs helps reproduce flaky tests and diagnose concurrency bugs. For current support and usage, see the documentation below.
