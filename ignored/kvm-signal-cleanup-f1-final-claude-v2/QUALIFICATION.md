Exact head `33d71aa0b02ca0a3183314e6d8db8696ad117191`, tree `1e9fdd2e169655062f88adcac71ce039b24f20fe`, has a tracked-clean source-bound receipt at `../kvm-signal-cleanup-f1-qualification-v3/receipt.json`, SHA256 `727c3c7763461b1d6907e063a7cb28b7785e856247ddebb732f6a07da76fb952`.

The receipt records equal pre/post head, tree, tracked status, changed-path set, and per-file SHA256. All eight phases have raw exit 0 and separate retained stdout/stderr hashes:

- focused KVM-required completion module: 9 passed, 0 failed/ignored, 736 filtered;
- full KVM-required library, ordinary parallelism: 745 passed, 0 failed/ignored/filtered, 7.80 seconds inside the test binary;
- full KVM-required library, one test thread: 745 passed, 0 failed/ignored/filtered, 27.17 seconds inside the test binary;
- formatting;
- default library check;
- default library strict Clippy with `-D warnings`;
- non-test `native-test-support` library check;
- non-test `native-test-support` library strict Clippy with `-D warnings`.

Raw streams are alongside that receipt. These are author-run Reverie L0 checks. No exact-head Hermit consumer, guest parity, ptrace/KVM comparison, or full-profile receipt is claimed.
