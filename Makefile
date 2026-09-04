.DEFAULT_GOAL := build

PKG_CONFIG ?= pkg-config
PKG_CONFIG_MODULES := libunwind-ptrace liblzma
SUBMODULE_PROXY ?= $(shell command -v with-proxy 2>/dev/null)
SUBMODULE_GIT = $(SUBMODULE_PROXY) git

# The shellcheck gate's inputs are DERIVED FROM THE REPOSITORY, not listed here.
#
# They used to be a hand-maintained list of ten path patterns, and it covered 51 of
# the 161 live shell scripts in this repo. The 110 it missed were not exotic: they
# included scripts/hermit-box-run, the cgroup-boxing wrapper wired at four call sites
# and referenced by 31 tracked files, and all four .githooks/tests/*.sh -- the tests
# for the very hooks the gate did check. So the list did not just miss new
# directories, it missed files inside directories it already claimed to cover, and
# nothing reported that: the gate passed, having looked at ten patterns rather than
# at the repository.
#
# scripts/list-shell-scripts.sh owns the selection rule and the exclusions, each of
# which is named and justified there rather than being an unexplained absence.
SHELLCHECK_TARGETS = $$(scripts/list-shell-scripts.sh)
#
# Same rule, same reason, for the other two languages. `scripts/*.rs` checked 19 of
# the 36 Rust sources this repository owns and `scripts/*.py` checked 41 of its 361
# Python files -- including none of ci-hub/, the machinery that decides whether a
# change may land, and none of the Python tools that carry no .py extension.
RUSTFMT_TARGETS    = $$(scripts/list-rust-sources.sh)
PYTHON_TARGETS     = $$(scripts/list-python-files.sh)

.PHONY: build build-full build-hermit check-agent-utils-pin check-hermit-agent-utils-pin check-claude-md-size check-codex-setup check-compat-envelope-tests check-deps check-harness-help check-portability check-primary-freshness check-rust-error-string-proxies clean \
	restore-primary-freshness \
	check-submodules checkout-all checkout-e9patch checkout-fresh checkout-optional-submodules checkout-sabre submodules \
	compat-envelope compat-envelope-full compat-envelope-fullcorpus \
	demo1 demo2 demo3 demo4 demo5 demo6 demo7 demos distclean doctor \
	doctor-core doctor-full doctor-qemu help init-hermit install-deps install-hooks \
	install-deps-core install-deps-full install-deps-qemu lint list-rust-scripts validate

.PHONY: single-submodule-bump

build: init-hermit
	@$(MAKE) --no-print-directory doctor-core
	@$(MAKE) --no-print-directory build-hermit

# Build the default Hermit feature set, including the DBI backend. This is
# intentionally separate from the lightweight ptrace/default demo build.
build-full: init-hermit
	@$(MAKE) --no-print-directory doctor-full
	cd hermit && cargo build --release -p hermit --bin hermit

# Wipe stale QEMU/Linux demo results (anchors, run history, snapshots) so the
# next demo run starts fresh. distclean also removes the kernel download and
# built initramfs. Both delegate to demos/clean.sh (honors QEMU_ASSETS).
clean:
	@demos/clean.sh

distclean:
	@demos/clean.sh --distclean

# Keep the repository-root entry points thin: demos/Makefile owns the artifact
# graph, including the phase-5 snapshot consumed by demos 6 and 7.
demo1 demo2 demo3 demo4 demo5 demo6 demo7:
	@$(MAKE) -C demos --no-print-directory $@

demos:
	@$(MAKE) -C demos --no-print-directory all

init-hermit: check-submodules

checkout-e9patch:
	@scripts/checkout-optional-submodules.rs e9patch

checkout-sabre:
	@scripts/checkout-optional-submodules.rs sabre

checkout-optional-submodules:
	@scripts/checkout-optional-submodules.rs all

# submodules: safe init/update that keeps every primary ATTACHED. Prefer this
# over `checkout-all` / `init-hermit`: the raw `git submodule update --init
# --recursive` below checks each `update = checkout` product out at its pinned
# gitlink in DETACHED HEAD, silently detaching a primary that was on main (or,
# for liteinst2, its feature branch). scripts/submodules.sh reattaches instead
# and never resets a dirty or divergent checkout. Pass ARGS=... to forward flags
# (e.g. `make submodules ARGS=--no-pull`).
submodules:
	@scripts/submodules.sh $(ARGS)

# checkout-all: raw recursive init. WARNING: this DETACHES attached primaries
# (see `make submodules` for the attach-preserving equivalent). Kept for the
# nested product checkout-all recursion below and legacy call sites.
checkout-all:
	@$(SUBMODULE_GIT) submodule update --init --recursive
	@$(MAKE) -C hermit --no-print-directory checkout-all
	@$(MAKE) -C reverie --no-print-directory checkout-all

checkout-fresh: ## Refresh clean primaries and publish one coherent parent snapshot
	@scripts/primary_checkout.py fresh --publish-parent --strict

check-submodules: checkout-all
	@status="$$($(SUBMODULE_GIT) submodule status --recursive)"; \
		printf '%s\n' "$$status"; \
		if printf '%s\n' "$$status" | awk '!($$2 == "agent-utils" && /^-/)' | grep -Eq '^[-+U]'; then \
			echo 'ERROR: a required submodule is missing or not at its pinned revision.' >&2; \
			exit 1; \
		fi
	@test -f hermit/Cargo.toml || { echo 'ERROR: Hermit submodule is missing.' >&2; exit 1; }
	@test -f reverie/Cargo.toml || { echo 'ERROR: Reverie submodule is missing.' >&2; exit 1; }
	@test -f liteinst2/Cargo.toml || { echo 'ERROR: LiteInst2 submodule is missing.' >&2; exit 1; }

build-hermit: init-hermit
	@if [ ! -f hermit/Cargo.toml ]; then \
		echo "ERROR: Hermit submodule checkout did not produce hermit/Cargo.toml." >&2; \
		exit 1; \
	fi
	@command -v cargo >/dev/null 2>&1 || { \
		echo "ERROR: cargo is required to build Hermit." >&2; \
		exit 1; \
	}
	cd hermit && cargo build --release -p hermit --bin hermit --no-default-features

check-deps:
	@set -eu; \
	pkg_config="$(PKG_CONFIG)"; \
	if ! command -v "$$pkg_config" >/dev/null 2>&1 \
		&& [ "$$pkg_config" = "pkg-config" ] \
		&& command -v pkgconf >/dev/null 2>&1; then \
		pkg_config="pkgconf"; \
	fi; \
	if ! command -v "$$pkg_config" >/dev/null 2>&1; then \
		echo "WARNING: pkg-config (pkgconf on CentOS/RHEL) is required." >&2; \
		echo "Run: make install-deps" >&2; \
		exit 1; \
	fi; \
	missing=""; \
	for module in $(PKG_CONFIG_MODULES); do \
		if ! "$$pkg_config" --exists "$$module"; then \
			missing="$$missing $$module"; \
		fi; \
	done; \
	if [ -n "$$missing" ]; then \
		echo "WARNING: missing required build dependencies:$$missing" >&2; \
		echo "libunwind-ptrace is provided by libunwind-dev (Debian/Ubuntu)" >&2; \
		echo "or libunwind-devel (CentOS/RHEL/Fedora)." >&2; \
		echo "liblzma is provided by liblzma-dev (Debian/Ubuntu)" >&2; \
		echo "or xz-devel (CentOS/RHEL/Fedora)." >&2; \
		echo "Run: make install-deps" >&2; \
		exit 1; \
	fi; \
	echo "Dependency check passed: $(PKG_CONFIG_MODULES)"

check-portability:
	@scripts/check-portable-paths.sh

check-harness-help: ## Assert every harness entrypoint's -h/--help/--version is a pure safe probe
	@scripts/check-harness-help.py

check-primary-freshness: ## One invariant over every primary (parent included): not bare, on main, not detached, equal to origin, clean. Detect+report only; never resets or fast-forwards.
	@scripts/primary_checkout.py freshness

restore-primary-freshness: ## Repair only the unambiguous drift (an accidental core.bare flip); everything else is reported with the exact command for a human.
	@scripts/primary_checkout.py freshness --restore-safe

check-rust-error-string-proxies: ## Reject Rust control flow that classifies typed errors by display strings
	@scripts/lint-rust-error-string-proxies.py . hermit reverie liteinst2

check-agent-utils-pin: ## Fetch and require exact Hermit-nested agent-utils pin equality; canonical checkout stays on main, isolated clones may be clean+detached
	@scripts/check-agent-utils-pin.rs

check-hermit-agent-utils-pin: ## Bound how far HERMIT's own recorded agent-utils gitlink trails agent-utils main
	@scripts/check_hermit_agent_utils_pin.py

# Keep the root policy compact even though `.codex/config.toml` raises Codex's
# instruction-chain limit. Stock Codex defaults to 32768 bytes and would truncate
# this file; `check-codex-setup` verifies the explicit project override. Policy is
# now SPLIT: executable predicates
# live in AGENTS.md, rationale/examples/glossary in ai_docs/agents-md-policy-rationale.md
# (read on demand). This gate is a REGRESSION guard set just above the trimmed size to
# catch bloat (e.g. re-inlining rationale that belongs in the companion doc). It also
# requires the load-verification TAIL CANARY, so a truncated tail fails loudly. If
# genuinely-required new policy pushes past LIMIT, move background to the companion doc
# first; raise LIMIT deliberately here only for irreducible new predicates, never by
# dropping a rule to fit a number.
# 2026-08-08: 42000 -> 42600. Raised deliberately for ONE irreducible new predicate, the
# owner-authorized RED-TIP DIRECT LAND exception, which had no prior home and whose absence
# blocked a critical path. The 2026-08-08 contradiction sweep first paid for most of it by
# DELETING superseded policy (coordinator-only closure, the "one agent in the parent" rule)
# and moving merge-gate measurements to the companion doc; the raise covers only the residue.
# 2026-08-08 (later, +206): 42600 -> 42800. ONE irreducible predicate added to that same
# exception: ESTABLISH THE RED YOURSELF before invoking it. Without it the carve-out is
# invocable on an unverified dispatch claim, which is the one way it can do harm — a PR
# bypass fired at a tip that is actually green. The worked example that motivates it went to
# the companion doc, per this file's own predicates-here/rationale-there split, so only the
# predicate itself is charged to AGENTS.md.
# 2026-08-08 (third, +1700): 42800 -> 44500. Hard Invariant 16, VERIFY BEFORE YOU REPLACE, plus
# its section. This is the largest single raise and the one most worth the bytes: the mission
# mandate authorizes close+respawn with no permission, so it paired an UNVERIFIED alarm with an
# IRREVERSIBLE action and nothing sat between them. A /proc census the same day found 29 live
# agents where the snapshot health-tick reads listed 13, i.e. 16 standing authorizations to
# destroy a live agent. The rule needs the invocation, both refusal codes, and the measurement
# that shows why a FRESHER signal would not have helped (only an INDEPENDENT one does) -- drop
# any of those and it reads as advice. Rationale beyond that went to the companion doc.
# NOTE FOR THE NEXT AUTHOR: three raises in one day is a smell, and the file is now dense
# predicates with little fat left. Before raising a fourth time, look for a SECTION to retire
# rather than sentences to shave.
check-claude-md-size: ## Guard AGENTS.md against size regression + require the tail canary
	@limit=44500; f=AGENTS.md; \
	test -f $$f || { echo "ERROR: $$f missing" >&2; exit 1; }; \
	size=$$(wc -c < $$f); \
	if [ $$size -gt $$limit ]; then \
	  echo "ERROR: $$f is $$size bytes, over the $$limit-byte regression guard (harness warns at 40000 chars)." >&2; \
	  echo "  Move background to ai_docs/agents-md-policy-rationale.md; raise LIMIT only for irreducible predicates, never drop a rule to fit." >&2; \
	  exit 1; \
	fi; \
	grep -q 'TAIL-CANARY-KESTREL-7731' $$f || { echo "ERROR: $$f missing the load-verification TAIL CANARY (tail may be truncated)." >&2; exit 1; }; \
	echo "AGENTS.md size OK ($$size <= $$limit chars) and tail canary present."

check-codex-setup: ## Verify stock-Codex instruction and skill discovery
	@python3 scripts/check-codex-setup.py

list-rust-scripts: ## Inventory executable Rust and rust-script source files
	@scripts/list-rust-scripts.rs

single-submodule-bump: ## Plan/run one isolated parent product gitlink A→B verification (ARGS='...')
	@scripts/single-submodule-bump.rs $(ARGS)

lint: ## Lint parent-repository scripts, tests, paths, and submodule policy
	@command -v rustfmt >/dev/null 2>&1 || { echo 'ERROR: rustfmt is required.' >&2; exit 1; }
	@command -v shellcheck >/dev/null 2>&1 || { echo 'ERROR: shellcheck is required.' >&2; exit 1; }
	@command -v python3 >/dev/null 2>&1 || { echo 'ERROR: python3 is required.' >&2; exit 1; }
	@# EVERY GATE RUNS. Make aborts a recipe at the first nonzero line, which made
	@# this target report only its earliest failure and silently skip everything
	@# after it -- measured 2026-08-07: rustfmt failed on 6 unformatted scripts/*.rs,
	@# so shellcheck never ran, and the 133 discovered scripts/test_*.py tests never
	@# ran either. Newly wired tests were nominal, not enforced. A lint target that
	@# hides its own coverage is worse than a slow one, so each gate is run through
	@# `gate`, which records the failure and continues; the target still exits
	@# nonzero, with EVERY failing gate named at the end rather than just the first.
	@# A GATE THAT PASSED IS NOT A GATE THAT RAN. `gate` reports OK on exit 0,
	@# which cannot distinguish "checked 33 files" from "checked nothing" -- and
	@# shellcheck and py-compile are silent on success, so their OK printed zero
	@# evidence of work. Measured 2026-08-07: all four countable tools DO fail
	@# closed on empty input today (rustfmt rc=1, shellcheck rc=2, py_compile rc=1,
	@# unittest rc=5 "NO TESTS RAN"), so nothing is vacuous right now -- but that
	@# is a property of the tools, not of this target, and nothing binds it. One
	@# `shopt -s nullglob`, or a switch to a `find` that legitimately matches
	@# nothing, and an empty run starts reporting OK. `counted` binds it: it counts
	@# the inputs, REFUSES on zero, and prints the count beside the verdict so the
	@# log carries what was checked instead of asking to be trusted.
	@# `ignored-artifacts` is REPORT-ONLY on purpose, and it is a ratchet.
	@# check-ignored-experiment-artifacts.sh exists to make one silent failure
	@# loud: the repo-root `*.log` rule (.gitignore:92) swallows experiment
	@# evidence, and `git add` on an ignored path says nothing useful, so an
	@# agent believes a golden is stored while the repo has never seen it. The
	@# script landed at 8ce511d and was then invoked by NOTHING -- an inert
	@# guard, which is the failure mode it was written to prevent, applied to
	@# itself. It runs in report mode (rc=0) rather than --strict because 196
	@# such files span 47 experiment directories today and most run spools
	@# SHOULD stay ignored -- only their owners can say which are intended
	@# evidence. Gating on day one would be red for weeks and get muted, the
	@# same reasoning label_taxonomy.py records for its `--gate p01` default.
	@# Move to --strict once the 47 directories are triaged.
	@# `pin-invariant` is report-only for the same ratchet reason as
	@# ignored-artifacts: the invariant is violated on 3 of 4 legs TODAY
	@# (measured 2026-08-07 -- manifest 79517704 vs reverie gitlink dd3c178e,
	@# and both product gitlinks behind their origin/main), so --strict would
	@# be red from the first run. It reads RECORDED GITLINKS, not checkouts:
	@# a colleague who clones and runs `git submodule update --init` receives
	@# the gitlinks, so an invariant read off this box's working tree would
	@# pass or fail on state nobody else can see. Move to --strict once the
	@# pins are reconciled.
	@set -u; failures=''; \
	gate() { \
	  name="$$1"; shift; \
	  printf '\n===== lint gate: %s =====\n' "$$name"; \
	  if sh -c "$$*"; then \
	    printf 'lint gate OK       : %s\n' "$$name"; \
	  else \
	    rc=$$?; printf 'lint gate FAILED   : %s (rc=%s)\n' "$$name" "$$rc"; \
	    failures="$$failures $$name"; \
	  fi; \
	}; \
	counted() { \
	  name="$$1"; unit="$$2"; count_cmd="$$3"; shift 3; \
	  printf '\n===== lint gate: %s =====\n' "$$name"; \
	  n=$$(sh -c "$$count_cmd" 2>/dev/null || echo 0); \
	  if [ "$$n" -eq 0 ] 2>/dev/null || [ -z "$$n" ]; then \
	    printf 'lint gate FAILED   : %s (zero %s to check -- refusing to pass vacuously)\n' "$$name" "$$unit"; \
	    failures="$$failures $$name"; \
	    return; \
	  fi; \
	  if sh -c "$$*"; then \
	    printf 'lint gate OK       : %s (%s %s checked)\n' "$$name" "$$n" "$$unit"; \
	  else \
	    rc=$$?; printf 'lint gate FAILED   : %s (rc=%s, %s %s checked)\n' "$$name" "$$rc" "$$n" "$$unit"; \
	    failures="$$failures $$name"; \
	  fi; \
	}; \
	counted rustfmt      files 'scripts/list-rust-sources.sh | wc -l' 'rustfmt --edition 2021 --check $(RUSTFMT_TARGETS)'; \
	counted rust-scripts scripts 'scripts/lint-rust-scripts.sh --count' 'scripts/lint-rust-scripts.sh'; \
	gate rust-script-smoke-tests 'scripts/test-lint-rust-scripts.sh'; \
	counted shellcheck   files 'scripts/list-shell-scripts.sh | wc -l' 'shellcheck --severity=warning $(SHELLCHECK_TARGETS)'; \
	counted py-compile   files 'scripts/list-python-files.sh | wc -l' 'python3 -m py_compile $(PYTHON_TARGETS)'; \
	counted py-unittest  tests 'python3 -m unittest discover -s scripts -p "test_*.py" 2>&1 | sed -n "s/^Ran \([0-9]*\) test.*/\1/p"' 'python3 -m unittest discover -s scripts -p "test_*.py"'; \
	gate error-proxies   '$(MAKE) --no-print-directory check-rust-error-string-proxies'; \
	gate gitmodules      'scripts/check-parent-gitmodules.sh'; \
	gate agent-utils-pin 'scripts/check-agent-utils-pin.rs'; \
	gate hermit-au-pin   'scripts/check_hermit_agent_utils_pin.py'; \
	gate primary-fresh   'scripts/primary_checkout.py check'; \
	gate reset-cas       '.githooks/tests/test-reset-cas-guard.sh'; \
	gate shared-index    '.githooks/tests/test-shared-index-guard.sh'; \
	gate commit-attrib   '.githooks/tests/test-commit-msg-attribution.sh'; \
	gate codex-setup     '$(MAKE) --no-print-directory check-codex-setup'; \
	gate claude-md-size  '$(MAKE) --no-print-directory check-claude-md-size'; \
	gate portability     '$(MAKE) --no-print-directory check-portability'; \
	gate commit-trailer  'scripts/check_commit_attribution.py --commit HEAD'; \
	gate harness-help    '$(MAKE) --no-print-directory check-harness-help'; \
	gate ignored-artifacts 'scripts/check-ignored-experiment-artifacts.sh'; \
	gate pin-invariant   'scripts/check_reverie_pin_invariant.py'; \
	gate compat-envelope '$(MAKE) --no-print-directory check-compat-envelope-tests'; \
	echo; \
	if [ -n "$$failures" ]; then \
	  echo "lint: FAILED gates:$$failures"; \
	  exit 1; \
	fi; \
	echo 'lint: all gates passed'

check-compat-envelope-tests: ## Run the compat-envelope renderer unit tests (fixture-only; no hermit build)
	@compat-envelope/tests/run-all.sh

# compat-envelope: the cross-backend compatibility REGRESSION gate. Builds the
# RELEASE hermit binary with the in-process DBI backend and asserts every
# known-green compat cell stayed green (green-stays-green), refreshing the
# scorecard CSVs as a side effect. This is the portable, always-on lane:
# ptrace (golden denominator) + DBI. SaBRe (needs `make checkout-sabre` + build)
# and KVM/reverie (need a /dev/kvm runner) are honestly recorded as n/a /
# not-runnable when absent — never a false red — and are exercised by
# compat-envelope-full.
#
# The gate runs against the RELEASE binary via HERMIT_BIN: a debug build is
# ~5-10x slower and blows the harness per-test timeout on subprocess-heavy cells
# (e.g. python-io-subprocess-time verify: 90s timeout under debug, passes under
# release), which is also the binary hermit's own CI validates against.
compat-envelope: init-hermit
	cd hermit && cargo build --release -p hermit --bin hermit --features dbi
	HERMIT_BIN="$(CURDIR)/hermit/target/release/hermit" \
		compat-envelope/validate-envelope.sh --lane portable \
		--backends ptrace,dbi --no-reverie

# compat-envelope-full: the privileged superset lane. Adds the SaBRe backend and
# the reverie B1.5 ptrace-vs-KVM boundary (needs /dev/kvm + the counter
# launchers). Intended for the privileged self-hosted CI runner, not portable CI.
compat-envelope-full: init-hermit
	cd hermit && cargo build --release -p hermit --bin hermit --features third-party-backends
	cd reverie && cargo build --release -p reverie-examples \
		--bin counter1 --bin counter2 \
		--bin reverie-kvm-counter1 --bin reverie-kvm-counter2
	HERMIT_BIN="$(CURDIR)/hermit/target/release/hermit" \
		compat-envelope/validate-envelope.sh --lane portable --backends ptrace,dbi,sabre

# compat-envelope-fullcorpus: the LOCAL definition-of-done gate. The
# portable/privileged split (compat-envelope[-full]) exists for GitHub CI, where
# a runner may lack /dev/kvm or the third-party-backend feature build. On a
# fully-provisioned local box (this machine has /dev/kvm) the gate should instead
# measure the UNION — the FULL 235-cell verify corpus (214 compiled C + 21
# shell/interpreter cells) across EVERY backend the local binary can run — not
# the ~28-cell ci=true portable subset. Backends are auto-detected; a missing one
# is recorded n/a, never a false red. Ratchet-asserted (green-stays-green) against
# per-backend det floors measured at 82a8e853.
compat-envelope-fullcorpus: init-hermit
	cd hermit && cargo build --release -p hermit --bin hermit --features third-party-backends
	HERMIT_BIN="$(CURDIR)/hermit/target/release/hermit" \
		compat-envelope/collect-fullcorpus.sh

# validate: the outer-repo definition-of-done gate. Locally this is the FULL
# 235-cell cross-backend envelope (both lanes' union); the portable/privileged
# split is CI-only. Extend as other workspace-level checks are added.
validate: compat-envelope-fullcorpus

doctor:
	@scripts/doctor.sh all

doctor-core:
	@scripts/doctor.sh core

doctor-full:
	@scripts/doctor.sh full

doctor-qemu:
	@scripts/doctor.sh qemu

# Install this clone's Git hooks (core.hooksPath -> the durable common Git
# directory's dev-hermit-hooks dispatchers). Wired
# as a prerequisite of every install-deps profile so a fresh clone/worktree gets
# the blocking pin-drift + hygiene pre-commit hook WITHOUT a separate manual step.
# core.hooksPath is per-repository local config (not tracked), so it must be set
# once per clone; linked worktrees share the installed dispatcher copies.
install-hooks:
	@scripts/setup-hooks.sh

# Backwards-compatible alias: the historical install-deps target is the
# lightweight core ptrace profile.
install-deps: install-deps-core

install-deps-core: install-hooks
	@scripts/install-deps.sh core
	@$(MAKE) --no-print-directory doctor-core

install-deps-full: install-hooks
	@scripts/install-deps.sh full
	@$(MAKE) --no-print-directory doctor-full

install-deps-qemu: install-hooks
	@scripts/install-deps.sh qemu
	@$(MAKE) --no-print-directory doctor-qemu

help:
	@echo "make install-deps[-core]  Install the lightweight ptrace profile"
	@echo "make install-deps-full    Install core + DBI/CMake dependencies"
	@echo "make install-deps-qemu    Install core + QEMU demo dependencies"
	@echo "make doctor               Check every profile without modifying the host"
	@echo "make doctor-{core,full,qemu}  Check one dependency profile"
	@echo "make check-deps           Verify required native pkg-config modules"
	@echo "make check-portability  Reject owner-specific paths in build/run files"
	@echo "make check-agent-utils-pin  Require Hermit's nested pin and resolved agent-utils checkout at origin/main"
	@echo "make check-hermit-agent-utils-pin  Bound how far hermit's own agent-utils gitlink trails main"
	@echo "make list-rust-scripts Inventory executable Rust and rust-script source files"
	@echo "make single-submodule-bump  Plan/run one isolated parent product gitlink A→B verification"
	@echo "make lint               Lint parent scripts, tests, paths, and submodule policy"
	@echo "make compat-envelope    Cross-backend compat regression gate (ptrace+DBI, portable CI lane)"
	@echo "make compat-envelope-full  Privileged superset (adds SaBRe + KVM/reverie, privileged CI lane)"
	@echo "make compat-envelope-fullcorpus  LOCAL full 235-cell union across all runnable backends"
	@echo "make validate           Outer-repo definition-of-done gate (local = full-corpus envelope)"
	@echo "make submodules         Safe init/update; keeps primaries ATTACHED (no detach)"
	@echo "make checkout-all       Recursive submodule init (WARNING: detaches primaries)"
	@echo "make checkout-fresh     Refresh clean primaries and publish parent gitlinks"
	@echo "make checkout-e9patch   Check out the optional pinned e9patch source"
	@echo "make checkout-sabre     Check out the optional pinned SaBRe source"
	@echo "make checkout-optional-submodules  Check out both optional sources"
	@echo "make / make build         Build lightweight ptrace Hermit"
	@echo "make build-full           Build default features including DBI"
	@echo "make demo1 .. demo7       Run one dependency-aware demo"
	@echo "make demos                Run every checked-in demo in order"
	@echo "make clean         Wipe stale QEMU/Linux demo results (fresh start)"
	@echo "make distclean     Also remove the demo kernel download + initramfs"
