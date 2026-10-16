/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The backend's entry-driver operations, moved here unchanged from
//! reverie-kvm-core's `entry::driver` module (step U6b of the provider seam):
//! an inherent impl of [`KvmBackend`] must live in reverie-kvm, which defines
//! it.

use crate::Result;
use crate::entry::driver::EntryDriverWatch;
use crate::entry::driver::check_entry;
use crate::entry::driver::combine_pending;
use crate::entry::driver::fold_causes;
use crate::entry::driver::route_foreign;
use crate::entry::owner::DriverLifecycle;
use crate::entry::owner::DriverOwner;
use crate::entry::owner::DriverScope;
use crate::vm::KvmBackend;

impl KvmBackend {
    pub fn entry_driver_watch(&self) -> EntryDriverWatch {
        EntryDriverWatch {
            owner: self.entry_driver_owner(),
            gate: self.memory.entry_gate(),
        }
    }

    pub fn check_entry_owner(&self) -> Result<()> {
        check_entry(self.entry_driver_ref(), self.memory.entry_gate_ref())
    }

    /// Call only after this driver's ordinary callback borrow scope ended.
    /// This yields for foreign publication; it never waits for physical joins
    /// or runs another driver's effects, hook, or publisher.
    pub async fn route_entry_outcome<R>(&self, result: Result<R>) -> Result<R> {
        let owner = self.entry_driver_owner();
        let owned = owner.as_ref().map(DriverOwner::pending).unwrap_or_default();
        for failure in &owned {
            if let Some(operation) = &failure.operation {
                operation.wait_callback_drop().await?;
            }
        }
        let result = fold_causes(result, owned.iter().flat_map(|failure| failure.causes()));
        let Some(failure) = self.memory.entry_gate().pending_failure() else {
            return result;
        };
        if failure.operation.as_ref().is_none_or(|origin| {
            owner
                .as_ref()
                .is_some_and(|owner| origin.same_driver(&owner.origin()))
        }) {
            return fold_causes(result, failure.causes());
        }
        route_foreign(result, &failure).await
    }

    /// Close registration after the last owned operation and route. Retain
    /// live causes in RunFailure until all worker cleanup has completed.
    pub fn finish_entry_driver<R>(&mut self, scope: DriverScope, result: Result<R>) -> Result<R> {
        let retirement = scope.retire();
        let result = fold_causes(
            result,
            retirement
                .pending
                .iter()
                .flat_map(|failure| failure.causes()),
        );
        if let Some(context) = &self.tool_failure {
            for failure in retirement.pending {
                context.run.retain_entry_failure(failure);
            }
        }
        let result = combine_pending(result, retirement.result);
        debug_assert!(matches!(
            retirement.lifecycle,
            DriverLifecycle::Retired | DriverLifecycle::Abandoned
        ));
        retirement.notification.notify();
        self.entry_driver = None;
        self.set_operation_origin(None);
        result
    }
}
