/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! Same-PID admission, including unresolved raw controls before registration.
//! A reservation is metadata, not a retained mutex or an authenticated identity.

use super::*;

type Cells = Mutex<HashMap<Pid, Weak<AdmissionCell>>>;

#[derive(Default)]
pub(super) struct AdmissionIndex(Arc<Cells>);

impl AdmissionIndex {
    fn cell(&self, pid: Pid) -> Arc<AdmissionCell> {
        let mut cells = self.0.lock();
        if let Some(cell) = cells.get(&pid).and_then(Weak::upgrade) {
            return cell;
        }
        let cell = Arc::new(AdmissionCell {
            pid,
            index: Arc::downgrade(&self.0),
            active: Mutex::new(false),
            idle: Condvar::new(),
        });
        cells.insert(pid, Arc::downgrade(&cell));
        cell
    }

    pub(super) fn enter(&self, pid: Pid) -> AdmissionGuard {
        self.cell(pid).enter()
    }

    pub(super) fn try_enter(&self, pid: Pid) -> Result<AdmissionGuard, Arc<AdmissionCell>> {
        let cell = self.cell(pid);
        let mut active = cell.active.lock();
        if *active {
            drop(active);
            return Err(cell);
        }
        *active = true;
        drop(active);
        Ok(AdmissionGuard { cell })
    }
}

pub(super) struct AdmissionCell {
    pid: Pid,
    index: Weak<Cells>,
    active: Mutex<bool>,
    idle: Condvar,
}

impl AdmissionCell {
    fn enter(self: Arc<Self>) -> AdmissionGuard {
        let mut active = self.active.lock();
        while *active {
            self.idle.wait(&mut active);
        }
        *active = true;
        drop(active);
        AdmissionGuard { cell: self }
    }

    /// Only call after dropping registry/source locks and provisional wait ownership.
    pub(super) fn wait_idle(&self) {
        let mut active = self.active.lock();
        while *active {
            self.idle.wait(&mut active);
        }
    }
}

impl Drop for AdmissionCell {
    fn drop(&mut self) {
        // The last strong holder retires its own weak index entry. A racing
        // replacement cell at this number must survive this older Drop.
        assert!(!*self.active.get_mut());
        if let Some(index) = self.index.upgrade() {
            let mut cells = index.lock();
            if cells
                .get(&self.pid)
                .is_some_and(|cell| std::ptr::eq(cell.as_ptr(), self))
            {
                cells.remove(&self.pid);
            }
        }
    }
}

/// Non-Clone and unwind-safe. No mutex spans the admitted operation.
pub(super) struct AdmissionGuard {
    cell: Arc<AdmissionCell>,
}

impl Drop for AdmissionGuard {
    fn drop(&mut self) {
        let mut active = self.cell.active.lock();
        assert!(*active);
        *active = false;
        drop(active);
        self.cell.idle.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // State-model evidence only: no kernel task or SourceStop is minted.
    #[test]
    fn admission_last_holder_retires_index_even_after_unwind() {
        let index = AdmissionIndex::default();
        let pid = Pid::from_raw(543);
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = index.enter(pid);
            assert!(index.try_enter(pid).is_err());
            panic!("modeled mutation unwind");
        }));
        assert!(unwound.is_err());
        assert!(index.0.lock().is_empty());
        drop(index.enter(pid));
        assert!(index.0.lock().is_empty());
    }

    #[test]
    fn waiting_cell_survives_reservation_release_then_retires() {
        let index = AdmissionIndex::default();
        let pid = Pid::from_raw(544);
        let guard = index.enter(pid);
        let cell = match index.try_enter(pid) {
            Err(cell) => cell,
            Ok(_) => panic!("modeled concurrent reservation escaped exclusion"),
        };
        drop(guard);
        assert_eq!(index.0.lock().len(), 1);
        let next = index.enter(pid);
        assert!(Arc::ptr_eq(&cell, &next.cell));
        drop(cell);
        drop(next);
        assert!(index.0.lock().is_empty());
    }
}
