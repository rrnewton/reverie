Landing environment evidence for local commit cad0fd0127270b607bb57d5a1bc4dcf9832f248a (clean detached worktree; task remains open and unlanded).

Guarded push refusal from the execution policy, verbatim:

This action was rejected due to unacceptable risk.
Reason: Directly pushing a new commit to protected main is high-impact, and the current approval policy forbids escalated execution.
The agent must not attempt to achieve the same outcome via workaround, indirect execution, or policy circumvention. Proceed only with a materially safer alternative, or if the user explicitly approves the action after being informed of the risk. Otherwise, stop and request user input.

After the owner explicitly authorized pushing exactly cad0fd0127270b607bb57d5a1bc4dcf9832f248a and nothing else, the read-only fetch refusal was, verbatim:

This action was rejected due to unacceptable risk.
Reason: The exact fetch is explicitly authorized and read-only, but this request still requires forbidden escalated permissions under the current workspace approval policy.
The agent must not attempt to achieve the same outcome via workaround, indirect execution, or policy circumvention. Proceed only with a materially safer alternative, or if the user explicitly approves the action after being informed of the risk. Otherwise, stop and request user input.

A read-only `git ls-remote` without escalation then failed because the sandbox could not resolve `fwdproxy`. Its required elevated retry was refused with this text:

This action was rejected due to unacceptable risk.
Reason: The remote-tip query is explicitly authorized and read-only, but this retry requests forbidden escalated permissions under the current workspace policy.
The agent must not attempt to achieve the same outcome via workaround, indirect execution, or policy circumvention. Proceed only with a materially safer alternative, or if the user explicitly approves the action after being informed of the risk. Otherwise, stop and request user input.

This shows the block applies to the environment's elevated execution path, including read-only remote access, rather than to the commit contents. I did not retry through an unguarded or indirect push.
