/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! SCRATCH MEASUREMENT ONLY. Never part of a landed commit.
//!
//! A [`Guard`] created at the top of a thread's closure records the stack
//! pointer, and when it drops, reports the thread's stack high-water mark to
//! the file named by `REVERIE_KVM_STACK_REPORT`: the lowest resident page of
//! the stack mapping (mincore), relative to the entry stack pointer and to the
//! top of the mapping. A fresh anonymous stack is untouched below the entry
//! frame; `reused` reports any resident bytes found there at entry (a stack
//! that glibc reused from its cache), which would make the mark an upper bound.
//! `REVERIE_KVM_PROBE_STACK` overrides the size `supervisor_thread::builder`
//! requests.

use std::io::Write;

const PAGE: usize = 4096;

/// The probe build's override of the supervisor stack size, if any.
pub fn override_bytes() -> Option<usize> {
    std::env::var("REVERIE_KVM_PROBE_STACK").ok()?.parse().ok()
}

#[inline(never)]
fn current_sp() -> usize {
    let marker = 0u8;
    std::hint::black_box(&marker) as *const u8 as usize
}

fn region_containing(address: usize) -> Option<(usize, usize)> {
    let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
    for line in maps.lines() {
        let range = line.split_whitespace().next()?;
        let (start, end) = range.split_once('-')?;
        let start = usize::from_str_radix(start, 16).ok()?;
        let end = usize::from_str_radix(end, 16).ok()?;
        if start <= address && address < end {
            return Some((start, end));
        }
    }
    None
}

/// The lowest resident page in [start, end), or `None` if none is resident.
fn lowest_resident(start: usize, end: usize) -> Result<Option<usize>, i32> {
    if end <= start {
        return Ok(None);
    }
    let pages = (end - start) / PAGE;
    let mut residency = vec![0u8; pages];
    // SAFETY: mincore only writes `pages` bytes into the vector.
    let rc = unsafe {
        libc::mincore(
            start as *mut libc::c_void,
            pages * PAGE,
            residency.as_mut_ptr(),
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(-1));
    }
    Ok(residency
        .iter()
        .position(|byte| byte & 1 != 0)
        .map(|index| start + index * PAGE))
}

struct Probe {
    label: &'static str,
    entry_sp: usize,
    start: usize,
    end: usize,
    reused: usize,
}

/// Reports this thread's stack high-water mark when dropped.
pub struct Guard(Option<Probe>);

impl Guard {
    pub fn new(label: &'static str) -> Self {
        if std::env::var_os("REVERIE_KVM_STACK_REPORT").is_none() {
            return Self(None);
        }
        let entry_sp = current_sp();
        let Some((start, end)) = region_containing(entry_sp) else {
            return Self(None);
        };
        let check_end = entry_sp.saturating_sub(64 * 1024) & !(PAGE - 1);
        let reused = match lowest_resident(start, check_end) {
            Ok(Some(low)) => check_end - low,
            Ok(None) => 0,
            Err(_) => usize::MAX,
        };
        Self(Some(Probe {
            label,
            entry_sp,
            start,
            end,
            reused,
        }))
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let Some(probe) = self.0.take() else {
            return;
        };
        let Some(path) = std::env::var_os("REVERIE_KVM_STACK_REPORT") else {
            return;
        };
        let (lowest, error) = match lowest_resident(probe.start, probe.end) {
            Ok(Some(low)) => (low, 0),
            Ok(None) => (probe.end, 0),
            Err(errno) => (probe.end, errno),
        };
        let thread = std::thread::current();
        // SAFETY: gettid has no preconditions.
        let tid = unsafe { libc::gettid() };
        let line = format!(
            "label={} thread={} pid={} tid={} region={} above_entry={} below_entry={} total={} reused={} requested={} mincore_errno={}\n",
            probe.label,
            thread.name().unwrap_or("?"),
            std::process::id(),
            tid,
            probe.end - probe.start,
            probe.end - probe.entry_sp,
            probe.entry_sp.saturating_sub(lowest),
            probe.end - lowest,
            probe.reused,
            override_bytes().map_or_else(|| "default".to_owned(), |bytes| bytes.to_string()),
            error,
        );
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = file.write_all(line.as_bytes());
        }
    }
}
