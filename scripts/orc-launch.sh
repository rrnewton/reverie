#!/usr/bin/env bash
# Copyright (c) Meta Platforms, Inc. and affiliates.
# All rights reserved.
#
# Launch the dev-hermit ORC coordinator with CLI arguments that the locally
# installed agent binaries actually accept.
#
# WHY THIS SCRIPT EXISTS
#
# ORC forwards --claude-args / --codex-args verbatim into ORC_CLAUDE_EXTRA_ARGS /
# ORC_CODEX_EXTRA_ARGS, and dg appends them to EVERY spawn of that CLI type,
# after the flags ORC itself adds. For Codex, ORC unconditionally adds
#     --sandbox workspace-write --ask-for-approval on-request
# so anything we put in --codex-args must COMPOSE with those two. Three
# concrete traps, all verified against codex-cli 0.146.0 on 2026-08-06:
#
#   1. --dangerously-disable-linux-sandbox is a Meta *launcher* flag that is now
#      gated: "only allowed with installed binaries on corp/lab hosts, or from
#      development builds in buck-out." On this shared dev box it exits 1 at
#      process start, so every persistent Codex pane died before claiming work.
#      This script probes the local binary and only passes the flag where it is
#      still accepted.
#   2. --sandbox and --model are NOT repeatable ("cannot be used multiple
#      times"). Putting either in --codex-args collides with the flag ORC adds
#      (--sandbox) or with a per-spawn --model, killing the pane at startup.
#      So: no --sandbox and no --model here. Instead, select the model through
#      Codex's repeatable --config model=SLUG form. This makes the selection
#      process-visible without guessing whether a hidden config or the server
#      would otherwise choose it.
#   3. --dangerously-bypass-approvals-and-sandbox is rejected at runtime with
#      "cannot be used with '--ask-for-approval'", which ORC always adds.
#
# The surviving, ungated way to give Codex agents the out-of-workspace writes
# they need (the TaskGraph DB and its rolling log live under ~/.tg, and
# --sandbox workspace-write denies them) is --add-dir, which is repeatable and
# composes with everything above.

set -euo pipefail

readonly DEFAULT_DB="hermit"
readonly DEFAULT_CODEX_MODEL="gpt-5.6-sol"
readonly GATED_CODEX_SANDBOX_FLAG="--dangerously-disable-linux-sandbox"
# Assigned before `readonly` on purpose: combining them makes the shell
# report the assignment's status, not `cd`'s, so a failed cd would be masked
# (SC2155).
DEV_HERMIT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
readonly DEV_HERMIT_ROOT

# Agents share the owner's GitHub credential, so their comments must be marked
# at the mutation boundary.  Put the versioned gh shim ahead of the system gh
# for every agent ORC spawns.  Human shells that do not carry DG_AGENT_NAME pass
# through unchanged; fleet shells get agent+session attribution or fail closed.
export PATH="$DEV_HERMIT_ROOT/ci-hub/bin:$PATH"

# Directories outside the ORC workspace that Codex agents must be able to write.
# Override with a colon-separated ORC_CODEX_EXTRA_WRITABLE_DIRS.
default_writable_dirs() {
    printf '%s\n' "${HOME}/.tg"
}

writable_dirs() {
    if [[ -n ${ORC_CODEX_EXTRA_WRITABLE_DIRS:-} ]]; then
        printf '%s\n' "${ORC_CODEX_EXTRA_WRITABLE_DIRS}" | tr ':' '\n'
    else
        default_writable_dirs
    fi
}

# Does the locally installed codex still accept the gated launcher flag?
# --version short-circuits before the TUI starts, but AFTER the launcher gate,
# so this is a real test of the gate and not of clap parsing.
codex_accepts_gated_sandbox_flag() {
    local codex
    codex=$(command -v codex 2>/dev/null) || return 1
    [[ -n $codex ]] || return 1
    timeout 60 "$codex" "$GATED_CODEX_SANDBOX_FLAG" --version >/dev/null 2>&1
}

codex_model() {
    local model="${ORC_CODEX_MODEL:-$DEFAULT_CODEX_MODEL}"
    if [[ ! $model =~ ^[A-Za-z0-9][A-Za-z0-9._/+:~-]*$ ]]; then
        echo "orc-launch: ORC_CODEX_MODEL must be a nonempty CLI model slug, got '$model'" >&2
        return 2
    fi
    printf '%s' "$model"
}

codex_args() {
    local -a args=()
    local model
    model=$(codex_model) || return $?

    if [[ ${ORC_CODEX_FORCE_SANDBOX_FLAG:-} == 1 ]] || codex_accepts_gated_sandbox_flag; then
        args+=("$GATED_CODEX_SANDBOX_FLAG")
    else
        # Gate refused the flag: grant the specific writable roots instead.
        local dir
        while read -r dir; do
            [[ -n $dir ]] || continue
            args+=(--add-dir "$dir")
        done < <(writable_dirs)
    fi

    args+=(--dangerously-enable-internet-mode)
    # -c/--config IS repeatable, so this composes safely with per-spawn args.
    args+=(--config "model=$model")
    args+=(--config model_reasoning_effort=xhigh)

    printf '%s' "${args[*]}"
}

claude_args() {
    printf '%s' "--dangerously-skip-permissions --dangerously-enable-internet-mode"
}

usage() {
    cat <<'EOF'
Usage: scripts/orc-launch.sh [DB] [-- EXTRA_ORC_ARGS...]
       scripts/orc-launch.sh --print-codex-args
       scripts/orc-launch.sh --print-claude-args
       scripts/orc-launch.sh --self-test
       scripts/orc-launch.sh -h | --help

Launch ORC (default db: hermit) with --claude-args/--codex-args that the locally
installed binaries accept. Read the header comment before changing either set:
--sandbox and --model are not repeatable and must never appear here. Codex uses
ORC_CODEX_MODEL when set, otherwise gpt-5.6-sol, through --config model=SLUG.
EOF
}

self_test() {
    local rc=0 out model tail

    model="vendor/model:vNext+preview"
    out=$(ORC_CODEX_MODEL="$model" ORC_CODEX_FORCE_SANDBOX_FLAG=1 codex_args)
    [[ $out == *"--config model=$model"* ]] || {
        echo "self-test: explicit Codex model was not emitted" >&2
        rc=1
    }
    tail=${out#*"--config model=$model"}
    [[ $tail != *"--config model="* ]] || {
        echo "self-test: Codex model was emitted more than once" >&2
        rc=1
    }

    out=$(ORC_CODEX_FORCE_SANDBOX_FLAG=1 codex_args)
    [[ $out == *"--config model=$DEFAULT_CODEX_MODEL"* ]] || {
        echo "self-test: default Codex model was not emitted" >&2
        rc=1
    }

    if ORC_CODEX_MODEL='not,a,slug' ORC_CODEX_FORCE_SANDBOX_FLAG=1 \
        codex_args >/dev/null 2>&1; then
        echo "self-test: unsafe Codex model slug was accepted" >&2
        rc=1
    fi

    out=$(ORC_CODEX_FORCE_SANDBOX_FLAG=1 codex_args)
    [[ $out == *"$GATED_CODEX_SANDBOX_FLAG"* ]] || {
        echo "self-test: forced mode did not emit the gated flag" >&2
        rc=1
    }

    # Simulate a host where the launcher gate refuses the flag.
    codex_accepts_gated_sandbox_flag() { return 1; }
    out=$(ORC_CODEX_EXTRA_WRITABLE_DIRS="/nonexistent-a:/nonexistent-b" \
        ORC_CODEX_FORCE_SANDBOX_FLAG=0 codex_args)
    [[ $out == *"--add-dir /nonexistent-a"* && $out == *"--add-dir /nonexistent-b"* ]] || {
        echo "self-test: refused mode did not emit --add-dir grants" >&2
        rc=1
    }
    [[ $out != *"$GATED_CODEX_SANDBOX_FLAG"* ]] || {
        echo "self-test: refused mode still emitted the gated flag" >&2
        rc=1
    }
    [[ $out == *"--dangerously-enable-internet-mode"* ]] || {
        echo "self-test: refused mode dropped internet mode" >&2
        rc=1
    }

    # Neither non-repeatable flag may ever appear in the extra-args channel.
    for forbidden in "--sandbox" "--model"; do
        out=$(ORC_CODEX_FORCE_SANDBOX_FLAG=1 codex_args)
        [[ $out != *"$forbidden"* ]] || {
            echo "self-test: codex args contain non-repeatable $forbidden" >&2
            rc=1
        }
    done

    ((rc == 0)) && echo "orc-launch self-test: ok"
    return $rc
}

main() {
    case "${1:-}" in
        -h | --help)
            usage
            exit 0
            ;;
        --print-codex-args)
            codex_args
            echo
            exit 0
            ;;
        --print-claude-args)
            claude_args
            echo
            exit 0
            ;;
        --self-test)
            self_test
            exit $?
            ;;
    esac

    local db="${1:-$DEFAULT_DB}"
    [[ $# -gt 0 ]] && shift || true
    [[ ${1:-} == "--" ]] && shift || true

    local codex claude
    codex=$(codex_args)
    claude=$(claude_args)

    echo "orc --db $db" >&2
    echo "  --claude-args=$claude" >&2
    echo "  --codex-args=$codex" >&2

    exec orc --db "$db" --detach \
        --claude-args="$claude" \
        --codex-args="$codex" \
        --resume "$@"
}

main "$@"
