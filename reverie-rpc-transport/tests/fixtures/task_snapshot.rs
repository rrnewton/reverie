//! One proc task enumeration. Diagnostic evidence never changes the verdict.
use std::ffi::OsString;
use std::io;
use std::io::Read;

#[derive(Debug)]
struct TaskSnapshot {
    entries: Vec<Result<OsString, String>>,
    tids: Vec<u32>,
    errors: Vec<String>,
}

impl TaskSnapshot {
    fn from_entries(entries: impl IntoIterator<Item = io::Result<OsString>>) -> Self {
        let mut snapshot = Self {
            entries: Vec::new(),
            tids: Vec::new(),
            errors: Vec::new(),
        };
        for entry in entries {
            match entry {
                Ok(name) => {
                    match name
                        .to_str()
                        .and_then(|name| name.parse::<u32>().ok())
                        .filter(|tid| *tid != 0)
                    {
                        Some(tid) => snapshot.tids.push(tid),
                        None => snapshot
                            .errors
                            .push(format!("nonnumeric task entry: {name:?}")),
                    }
                    snapshot.entries.push(Ok(name));
                }
                Err(error) => {
                    let error = format!("task enumeration: {error:?}");
                    snapshot.errors.push(error.clone());
                    snapshot.entries.push(Err(error));
                }
            }
        }
        snapshot
    }

    fn read() -> Self {
        match std::fs::read_dir("/proc/self/task") {
            Ok(entries) => {
                Self::from_entries(entries.map(|entry| entry.map(|entry| entry.file_name())))
            }
            Err(error) => Self {
                entries: Vec::new(),
                tids: Vec::new(),
                errors: vec![format!("open /proc/self/task: {error:?}")],
            },
        }
    }

    fn is_single_owner(&self, owner: u32) -> bool {
        self.errors.is_empty() && self.entries.len() == 1 && self.tids == [owner]
    }
}

fn task_details(tid: u32, file: &str) -> String {
    // A missing task AFTER enumeration remains a failed assertion. Limit each
    // supplemental read; these bytes are diagnostic and never reclassify it.
    let result = (|| -> io::Result<_> {
        let mut bytes = Vec::new();
        std::fs::File::open(format!("/proc/self/task/{tid}/{file}"))?
            .take(513)
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    })();
    match result {
        Ok(mut bytes) => {
            let truncated = bytes.len() > 512;
            bytes.truncate(512);
            format!("bytes={bytes:?}, truncated={truncated}")
        }
        Err(error) => format!("error={error:?}"),
    }
}

pub fn assert_one_owner_task(
    site: &str,
    mode: &str,
    turn: usize,
    workers: impl FnOnce() -> String,
) {
    let snapshot = TaskSnapshot::read();
    let owner = std::process::id();
    if snapshot.is_single_owner(owner) {
        return;
    }
    let tid = unsafe { libc::syscall(libc::SYS_gettid) };
    eprintln!(
        "task invariant failure: site={site}, mode={mode}, turn={turn}, pid={owner}, tid={tid}, expected_tasks=1, snapshot={snapshot:?}, workers={}",
        workers()
    );
    for tid in snapshot.tids.iter().take(16) {
        for file in ["comm", "stat", "wchan"] {
            eprintln!(
                "post-snapshot tid={tid} {file}: {}",
                task_details(*tid, file)
            );
        }
    }
    eprintln!(
        "post-snapshot task details truncated={}",
        snapshot.tids.len() > 16
    );
    panic!("expected exactly one owner task at {site}: {snapshot:?}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(ids: &[&str]) -> TaskSnapshot {
        TaskSnapshot::from_entries(ids.iter().map(|name| Ok(OsString::from(name))))
    }

    #[test]
    fn only_one_valid_owner_passes() {
        assert!(names(&["100"]).is_single_owner(100));
        assert!(!names(&[]).is_single_owner(100));
        assert!(!names(&["101"]).is_single_owner(100));
        assert!(!names(&["100", "101"]).is_single_owner(100));
    }

    #[test]
    fn every_error_and_raw_name_is_retained_and_fails_closed() {
        let snapshot = TaskSnapshot::from_entries([
            Ok(OsString::from("100")),
            Err(io::Error::from_raw_os_error(libc::EIO)),
            Ok(OsString::from("not-a-tid")),
        ]);
        assert_eq!(snapshot.entries.len(), 3);
        assert_eq!(snapshot.tids, [100]);
        assert_eq!(snapshot.errors.len(), 2);
        assert!(!snapshot.is_single_owner(100));
        let single_error =
            TaskSnapshot::from_entries([Err(io::Error::from_raw_os_error(libc::EIO))]);
        assert!(!single_error.is_single_owner(100));
        assert!(!names(&["not-a-tid"]).is_single_owner(100));
        assert!(!names(&["0"]).is_single_owner(0));
    }

    #[test]
    fn joined_worker_identity_cannot_exempt_an_extra_entry() {
        // Deliberately no worker-observation input in the comparator: even a
        // successfully joined capture TID still fails this exact requirement.
        let successfully_joined_tid = 101;
        let snapshot = names(&["100", &successfully_joined_tid.to_string()]);
        assert_eq!(snapshot.tids, [100, successfully_joined_tid]);
        assert!(!snapshot.is_single_owner(100));
    }
}

#[cfg(test)]
mod live_test {
    #[test]
    fn live_extra_task_fails_with_diagnostics() {
        let (tid_sender, tid_receiver) = std::sync::mpsc::channel();
        let (release, held) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            tid_sender
                .send(unsafe { libc::syscall(libc::SYS_gettid) })
                .unwrap();
            let _ = held.recv();
        });
        let tid = tid_receiver.recv().unwrap();
        let observed = std::cell::Cell::new(false);
        let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            super::assert_one_owner_task("live negative control", "diagnostic-test", 0, || {
                observed.set(true);
                format!("synthetic held worker tid={tid}; not capture-owned")
            });
        }));
        release.send(()).unwrap();
        worker.join().unwrap();
        assert!(failure.is_err(), "a live extra task must still fail");
        assert!(observed.get(), "failure must request worker evidence");
    }
}
