# Hermit website overhaul — plan v1

For Ryan Newton, from writer. First round: page structure, visual direction, and complete proposed copy. Site and README implementation come after discussion.

**Recommendation:** explain the testing problem first, show how to try Hermit, then introduce the related software and link to current compatibility results. Keep the orbit animation and existing cream/coral/blue palette. Remove the numerical score strip and the maturity table from the landing page.

## 1. What the current page gets wrong

| Current section | Problem for a first-time reader | Proposed change |
| --- | --- | --- |
| Header and hero | “Hermetic Infra” introduces an umbrella name before explaining Hermit. “Open infrastructure for controlled execution” and “a typed instrumentation API” describe categories rather than a problem the reader recognizes. | Lead with Hermit and repeatable testing. Keep **Tools for Reproducible Linux Execution** as the title. Shorten “Read the Rust API” to **Rust API**. |
| Animation | “observe,” “schedule,” and “intercept” orbit Linux; “same input / same evolution” does not explain what Hermit does or where it sits. | Put Guest / Hermit / Linux in a vertical stack. Orbit the inputs that can make executions differ. |
| Score strip | “191/191 ptrace strict L2 corpus” requires knowledge of an internal test classification. The other ratios measure different things, and none tells a newcomer whether their program will work. Hard-coded counts age quickly. | Remove the strip. Put a prominent link to the original explanation here instead of numerical claims. |
| Toolchain | “Three layers, one boundary” implies a required stack without explaining the relationship. “Instruction punning” and “policy-free” are poor introductions to LiteInst. | Call this **Software components**. Say what each project does and who would use it; distinguish Hermit's instrumentation library from an optional patching mechanism. |
| Compatibility | “Measured, not implied,” “checked-in ratchets,” “ptrace-parity contracts,” and “B2.1” speak to project maintainers. TODO cells make an internal tracking table the main public explanation. | Explain the default execution backend briefly, then link to the latest compatibility scorecard. An optional chart must summarize actual test results. |
| What works now | “From a process to a kernel” and “Frontier demonstrations extend the same control loop” never identify the reader's task. The QEMU snapshot claim can suggest stronger reproducibility than the README's boot demonstration supports. | Introduce ordinary testing uses earlier. Move QEMU, btrfs, and drgn experiments into documentation reached through an Examples link. |
| Footer | “An initial project sketch” describes the website's construction rather than helping the reader. | Use a compact set of project and documentation links. |

## 2. Proposed information architecture

| Order | Section | Purpose and audience |
| --- | --- | --- |
| 1 | Hero + original blog post | Tell a developer arriving for the first time what Hermit does. Show the architecture and offer Get started / Rust API. Cite the blog directly beneath the introduction. |
| 2 | Why Hermit? | Explain implicit inputs through flaky tests and concurrency bugs. For developers deciding whether Hermit addresses their problem. |
| 3 | Try Hermit | Give an evaluator one small, understandable example and an installation link. |
| 4 | Software components | Help users choose Hermit and tool builders find Reverie or LiteInst. Keep the three-card layout. |
| 5 | Compatibility | Explain platform and execution choices, identify record/replay as experimental, and send evaluators to current results. |
| 6 | Footer | Direct returning visitors to source, documentation, examples, and issue reporting. |

This version makes Hermit the entry point. The other projects remain visible without making newcomers understand all three before trying anything.

## 3. Visual direction

### Hero animation

Keep the slow orbits, but replace the circular Linux core with three stationary, stacked rectangles:

```text
             ┌──────────────────────┐
             │ Guest                │
             │ Your Linux program   │
             ├──────────────────────┤
             │ Hermit               │
             │ Controls execution   │
             ├──────────────────────┤
             │ Linux                │
             │ Host kernel          │
             └──────────────────────┘
```

Use the coral accent for Hermit. Arrange six labels on the surrounding orbits: **Thread scheduling**, **Clocks**, **Random bytes**, **Process IDs**, **File metadata**, **CPU clock and features**. Keep the labels upright as they move. Their motion represents sources of variation; the central stack stays still. The diagram's caption is included in the proposed copy below.

These labels describe the default execution path. File metadata means selected observations such as inode identifiers, not file contents. Process IDs rely on the default isolated process namespace. CPU clock and features refer to RDTSC and CPUID; interception depends on the host. Do not imply control over every source of hardware randomness. Leave external networking, externally injected signals, and ASLR out of this first diagram: they would require qualifications that overwhelm it, and the inspected sources do not establish a general guest-address guarantee.

On phones, place a static stack and the six labels below the introduction. Honor reduced-motion preferences and provide a pause control for the animation. Keep the explanation available as text, with no information accessible only on hover.

### Compatibility: link first, optional chart

The public destination is **[Latest compatibility scorecard](https://rrnewton.github.io/hermit/compatibility/latest/)**. Keep that link visible even if no chart ships.

If we add a chart, use one horizontal bar per execution backend, under **Passing tests by execution backend**. The horizontal axis is the number of passing tests. Explain names next to them, for example “ptrace — Linux process tracing” and “KVM — hardware virtualization.” Link the chart and its caption to the scorecard.

Generate it from one dated ledger snapshot using the same named test population and comparison rule across backends. Show passes against the applicable total, distinguishing failures from missing results; exclude inapplicable cases explicitly. Include the Hermit revision, observation date, and a small accessible data table. Keep record/replay results separate from repeated-run results. If comparable data is unavailable, publish the link alone.

Do **not** use `SCORECARD.md`'s “Selected by full” totals as passing counts: that file explicitly describes test selection and applicability, not observed outcomes. Do not hand-maintain counts in HTML or combine tests that check only output with tests that also compare execution logs. My recommendation for the first site revision is the link alone.

## 4. Complete proposed page copy

The text below is the proposed public copy. “Header” and “Footer” identify page regions; the other headings are visible section headings. Diagram labels are specified above. The optional chart would add the title above and the caption below, with values generated from its source data.

### Header

**Hermit**

[Why Hermit?](#why-hermit) · [Get started](#try-hermit) · [Software components](#software-components) · [Compatibility](#compatibility)

### Tools for Reproducible Linux Execution

Hermit runs unmodified Linux programs in a controlled, repeatable environment. It controls inputs that can change a test's outcome: thread scheduling, time, and random numbers. Use it to reproduce failures, explore different thread schedules, and investigate concurrency bugs.

[Get started](#try-hermit) · [Rust API](https://rrnewton.github.io/hermit/hermit/index.html)

Read the original introduction: [Hermit: Deterministic Linux for Controlled Testing and Software Bug-finding](https://developers.facebook.com/blog/post/2022/11/22/hermit-deterministic-linux-testing/) — Ryan Rhodes Newton, Meta, November 2022.

*Diagram caption:* Hermit sits between your program and Linux, controlling system calls and processor events that can make executions differ.

### Why Hermit?

A test has more inputs than its arguments and files. Its result can also depend on which thread runs first, what the clock says, or which random bytes it receives. These implicit inputs can make a failure difficult to reproduce.

Hermit lets you control those inputs:

- **Repeat an execution.** Run a supported program again with the same inputs and settings to investigate a failure.
- **Look for concurrency bugs.** Change the scheduling seed to explore other thread interleavings. Keep the seed to reproduce an interesting run with the same program and Hermit build.
- **Narrow down a failure.** Compare passing and failing schedules to find changes in event order that trigger the bug.

Reproducible runs need a fixed program, input files, environment, and Hermit configuration, including the starting clock time. Responses from external services must also be controlled; Hermit does not make the external network repeatable.

### Try Hermit

[Install Hermit on x86-64 Linux](https://github.com/rrnewton/hermit#install-from-source), then try reading random bytes:

```sh
hermit run -- /bin/sh -c 'od -An -N8 -tx1 /dev/urandom'
```

Run the command twice. With the same Hermit build and random seed, it prints the same bytes. Hermit supplies repeatable data when the program asks Linux for randomness.

To run your own program, place `hermit run --` before its command. The [user guide](https://github.com/rrnewton/hermit/blob/main/docs/USER_GUIDE.md) explains host requirements, clock settings, and schedule exploration.

### Software components

Use Hermit to run programs, or use the libraries below to build your own tools.

#### Hermit

A command-line tool for repeatable execution and concurrency testing of x86-64 Linux programs. Hermit uses Reverie to intercept system calls and processor events, then controls scheduling, time, and other inputs to the program.

[Source](https://github.com/rrnewton/hermit) · [User guide](https://github.com/rrnewton/hermit/blob/main/docs/USER_GUIDE.md) · [Rust API](https://rrnewton.github.io/hermit/hermit/index.html)

#### Reverie

A Rust library for building tools that observe and change the behavior of Linux programs. Write handlers for system calls and processor events to inspect a program, change results, or perform additional operations. Hermit uses these handlers to implement deterministic execution.

[Source and guide](https://github.com/rrnewton/reverie)

#### LiteInst

A library for patching x86-64 machine instructions while a program runs. LiteInst2 brings the original C++ project's patching core to Rust. Hermit's experimental LiteInst integration uses this technique to intercept program operations.

[Original LiteInst](https://github.com/iu-parfunc/liteinst) · [LiteInst2 in Rust](https://github.com/rrnewton/liteinst2)

[Living on the edge — PLDI 2016](https://doi.org/10.1145/2908080.2908084) · [Instruction punning — PLDI 2017](https://doi.org/10.1145/3062341.3062344)

### Compatibility

Hermit supports x86-64 Linux. Which programs work depends on the system calls they use, the host machine, and how Hermit intercepts their execution.

These interception mechanisms are called execution backends. Start with the default, **ptrace**, which uses Linux's process-tracing interface and is the most thoroughly tested. Other backends are experimental or specialized.

The compatibility scorecard shows recorded test results across backends. Use it to find tested workloads and inspect the checks behind a result. A passing test covers the behavior it checks; try the workload you depend on.

[Latest compatibility scorecard](https://rrnewton.github.io/hermit/compatibility/latest/)

Record and replay are experimental and support fewer workloads than deterministic execution. See the [record/replay guide](https://github.com/rrnewton/hermit/blob/main/docs/USER_GUIDE.md#record-and-replay) for instructions and limitations.

*Caption if the optional chart is included:* Passing tests from a dated run of the same test suite across execution backends. Missing results are shown separately. See the scorecard for individual results and the checks each test performs.

### Footer

**Hermit**

[Source](https://github.com/rrnewton/hermit) · [Documentation](https://github.com/rrnewton/hermit#more-documentation) · [Examples](https://github.com/rrnewton/hermit/tree/main/examples) · [Report an issue](https://github.com/rrnewton/hermit/issues) · [Page source](https://github.com/rrnewton/hermit/tree/main/docs/site)

## 5. Proposed README addition

Add this paragraph immediately after “Hermit is useful for repeatable execution, controlled concurrency testing, record/replay experiments, and diagnosing schedule-sensitive failures,” before **Requirements**. That puts the conceptual introduction near the top rather than burying it in More Documentation.

> For an introduction to the ideas behind Hermit, read [Hermit: Deterministic Linux for Controlled Testing and Software Bug-finding](https://developers.facebook.com/blog/post/2022/11/22/hermit-deterministic-linux-testing/) by Ryan Rhodes Newton (Meta, November 2022). It explains how controlling thread schedules and other implicit inputs helps reproduce flaky tests and diagnose concurrency bugs. For current support and usage, see the documentation below.

## 6. Open questions for Ryan

1. **Identity:** should the header say **Hermit**, as proposed, or retain **Hermetic Infra** as an umbrella for the three projects?
2. **Examples:** should the landing page concentrate on testing, as proposed, or keep a short advanced-examples section for QEMU, btrfs, and drgn?
3. **Compatibility visual:** start with the scorecard link alone, or include the automatically generated bar chart in the first implementation?

## Source notes

- Current page, README, user guide, architecture guide, and scorecard inspected at Hermit `origin/main`, commit `18ece74f8b0f68ff57871c63320669424c750e15`. The [current page source](https://github.com/rrnewton/hermit/blob/18ece74f8b0f68ff57871c63320669424c750e15/docs/site/index.html) contains the phrases quoted above.
- The 2022 blog post was retrieved and read successfully on 2026-10-01. The proposed narrative draws on its explanation of implicit inputs, flaky tests, schedule exploration, and Reverie. Historical compatibility percentages and blanket guarantees are not carried into the new copy.
- The public scorecard URL returned HTTP 200. Its address appears in the current page and `.github/scripts/rebuild-compatibility-site.py`. [SCORECARD.md](https://github.com/rrnewton/hermit/blob/18ece74f8b0f68ff57871c63320669424c750e15/SCORECARD.md) distinguishes selection from observations.
- The random-byte example comes from the current README; it was not executed for this writing round. Current clock guidance requires an explicit `--epoch` to reproduce the starting time across separate invocations. The diagram claims were checked against the README and architecture documentation.
