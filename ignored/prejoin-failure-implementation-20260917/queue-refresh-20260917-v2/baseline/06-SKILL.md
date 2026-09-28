---
name: github-with-proxy
description: Use GitHub with git and gh from this user's proxy-restricted development hosts. Apply before GitHub fetch, pull, push, PR, or CI operations and when diagnosing network errors, proxy denials, or a session started without internet access. Distinguish transport, session permissions, GitHub authentication, and workflow approval; do not apply to ordinary offline Git operations.
---

# GitHub through the proxy

Use the installed `with-proxy` wrapper for networked Git and GitHub CLI commands
on this host. This is user-account guidance, independent of the current project.
It does not require a dev-hermit checkout and does not authorize new remote writes.

## Start with the actual operation

```bash
command -v with-proxy
git remote -v
with-proxy git ls-remote https://github.com/OWNER/REPO refs/heads/main
with-proxy gh pr list --repo OWNER/REPO
```

Use the repository the user requested, not the example names. Git's HTTPS path
uses `github.com`; `gh` also uses `api.github.com`. Success on one does not prove
the other works. Prefer a read-only operation before any mutation.

Typical commands:

```bash
with-proxy git fetch origin
with-proxy git pull --ff-only origin BRANCH
with-proxy gh pr view NUMBER --repo OWNER/REPO
with-proxy gh pr checks NUMBER --repo OWNER/REPO
with-proxy gh run list --repo OWNER/REPO
```

Inspect working-tree state before pulling; a dirty shared checkout is not a
landing workspace. Use a clean detached worktree for authorized reviews and
landing preparation when unrelated edits are present. Keep repository-specific
review, validation, branch, and human-approval rules in force.

## Classify failures before calling them blockers

- **No internet enabled for this agent session:** `with-proxy` does not grant
  internet permissions. Explain that the session must be restarted or enabled
  with network access. Once the user explicitly enables it or restarts, repeat
  the same small read-only Git and `gh` checks. A denial from the old session is
  not evidence about the restarted session.
- **Timeout, name-resolution failure, or network unreachable:** check that the
  actual command uses `with-proxy`, that the wrapper is present, and that the
  tool is using HTTPS. Retry a missing-wrapper invocation through the wrapper.
- **CONNECT 403 naming a destination filter/allowlist/agent identity:** the
  proxy was reached but refused this request. This is not proof of bad GitHub
  credentials. Keep the exact error as evidence; stop equivalent retries until
  permissions or session state change. Do not switch identities, alternate
  proxies, or tools to evade the denial. Ask the owner to enable the intended
  session or arrange the required access.
- **GitHub 401/403 after reaching GitHub:** distinguish account/token permissions
  from proxy policy using the actual response. Then inspect `gh auth status`
  without exposing tokens. Do not reauthenticate, rotate credentials, or change
  another session's identity merely because a proxy error was labeled an auth error.
- **No PR checks:** inspect workflow runs before assuming CI is green or broken.
  Fork workflows can be `action_required` awaiting approval. Check the workflow
  changes and existing review authorization before approving runs; approval is
  an external action, not a network workaround.

Do not infer a permanent outage from one observation. Conversely, do not keep
retrying an explicit denial without an actual change in permission/session state.

## Preserve the real result

Never infer push, merge, or validation success from `$?` after `| tail` or
`| tee`: that can be the output command's status. Capture the result first:

```bash
with-proxy git fetch origin > /tmp/github-fetch.log 2>&1
result=$?
if [ "$result" -ne 0 ]; then
    tail -30 /tmp/github-fetch.log
fi
```

For an authorized push or merge, verify the outcome independently using fetched
remote refs and the PR's current state/head. Do not delete branches, bypass
checks, or override a human review hold based only on a command's success message.

## Boundaries and portability

- The wrapper scopes proxy settings to the command. Do not install blanket
  global proxy settings, rewrite remotes, modify credentials, or change shell
  startup files unless the user requests that configuration change.
- HTTP proxy environment variables do not automatically proxy Git-over-SSH.
  Diagnose that transport explicitly rather than repeatedly wrapping an SSH URL.
- Keep the existing internal-host and localhost proxy bypass configuration.
- Do not print tokens or whole environment dumps in logs or examples.
- Keep host-specific access details in user-level skills, not project READMEs.
- On hosts without this wrapper, inspect their supported networking setup;
  do not fabricate a replacement proxy or assume direct egress is allowed.

Promoted from the working conventions in `~/work/dev-hermit/README.md`, its
`AGENTS.md` exit-status/landing guidance, and its proxy-prefixed Git/PR tooling.
Those repository files are provenance, not runtime dependencies of this skill.
