// Copyright (c) Meta Platforms, Inc. and affiliates.
// Licensed under the BSD-style license in the repository LICENSE file.
#[path = "dbt_admission.rs"]
mod tools;
reverie_dbt::export_caller_tool!(crate::tools::AdmissionTool);

#[cfg(test)]
mod tests {
    #[test]
    fn copied_runtime_refusal_is_not_a_guest_errno() {
        // Direct ABI/action control only: initial-root mode admits no copied
        // child. Native treats action 1 as unsupported and negatives as errno.
        let action = unsafe {
            super::__reverie_caller_runtime::reverie_dbt_runtime_copied_syscall(
                libc::SYS_getpid,
                std::ptr::null(),
            )
        };
        assert_eq!(action, 1);
    }
}
