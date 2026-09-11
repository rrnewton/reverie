//! Genuinely retained external artifacts.
//!
//! The superseded harness used `tempfile::tempdir()`. Its `Drop` runs
//! `remove_dir_all`, so unwinding from the very panic that said "streams retained
//! at <path>" deleted `guest.stdout`, `guest.stderr`, the server streams and
//! `timeout.status`. Successful returns and ordinary assertion failures deleted
//! them too.
//!
//! This helper writes into a caller-supplied external root and **never deletes**.
//! Its `Drop` records a terminal outcome instead, so an unwind is evidence rather
//! than erasure.

use std::cell::Cell;
use std::path::Path;
use std::path::PathBuf;

/// Names the external root. Absent is a hard failure, never a skip.
pub const ARTIFACTS_ENV: &str = "OWNED_PUBLIC_INSTRUCTION_ARTIFACTS";

/// A retained per-case directory. Dropping it records an unwind note; it removes
/// nothing.
///
/// Ordinary evidence writes propagate their `io::Error`, so a case cannot claim
/// success while a record is missing. Only the `Drop` path is best effort, because
/// a destructor must not unwind.
///
/// A terminal cause set through [`Retained::terminal`] is sticky: `Drop` records the
/// unwind separately and never overwrites it, so a timeout stays a timeout instead
/// of being relabelled with a generic panic.
/// Single-threaded interior mutability keeps the finalization flags settable
/// through a shared reference, so a borrower such as the coordinator handle can
/// coexist with a sticky terminal write. Evidence writes still propagate their
/// `io::Error`.
#[derive(Debug)]
pub struct Retained {
    directory: PathBuf,
    completed: Cell<bool>,
    terminal: Cell<bool>,
}

impl Retained {
    /// Create `<root>/<case>` under an explicit root. Fails if it already exists,
    /// so a rerun can never quietly overwrite earlier evidence.
    pub fn under(root: &Path, case: &str) -> std::io::Result<Self> {
        let directory = root.join(case);
        std::fs::create_dir_all(root)?;
        std::fs::create_dir(&directory)?;
        let retained = Self {
            directory,
            completed: Cell::new(false),
            terminal: Cell::new(false),
        };
        retained.record("location", &format!("{}\n", retained.directory.display()))?;
        retained.record("outcome", "started")?;
        Ok(retained)
    }

    /// Create under the root named by [`ARTIFACTS_ENV`].
    pub fn new(case: &str) -> std::io::Result<Self> {
        let root = std::env::var_os(ARTIFACTS_ENV).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{ARTIFACTS_ENV} must name an external retained artifact root"),
            )
        })?;
        Self::under(Path::new(&root), case)
    }

    pub fn path(&self) -> &Path {
        &self.directory
    }

    pub fn join(&self, name: &str) -> PathBuf {
        self.directory.join(name)
    }

    /// Write one named record, propagating any failure.
    pub fn record(&self, name: &str, value: &str) -> std::io::Result<()> {
        std::fs::write(self.directory.join(name), value)
    }

    /// Best-effort write for destructor use only, where unwinding is not allowed.
    ///
    /// The fallback diagnostic is a fallible `writeln!` whose result is deliberately
    /// discarded. `eprintln!` cannot be used here: it panics when stderr itself
    /// refuses the write, which would unwind out of a destructor and, during an
    /// existing unwind, abort the process and destroy the primary failure.
    pub fn record_best_effort(&self, name: &str, value: &str) {
        if let Err(error) = self.record(name, value) {
            use std::io::Write;
            let mut stderr = std::io::stderr().lock();
            let _ = writeln!(
                stderr,
                "retention: cannot write {name} under {}: {error}",
                self.directory.display()
            );
        }
    }

    /// Record a sticky terminal cause. `Drop` will not replace it.
    ///
    /// Use this on a path that is about to unwind for a known reason, such as the
    /// guest deadline, so the recorded cause survives the panic.
    pub fn terminal(&self, cause: &str) -> std::io::Result<()> {
        self.record("outcome", cause)?;
        self.terminal.set(true);
        Ok(())
    }

    /// Record the successful terminal outcome. Completion is claimed only when the
    /// write actually succeeded.
    pub fn finish(&self, outcome: &str) -> std::io::Result<()> {
        self.record("outcome", outcome)?;
        self.completed.set(true);
        self.terminal.set(true);
        Ok(())
    }

    /// Whether a terminal cause is already recorded and protected from `Drop`.
    pub fn has_terminal_cause(&self) -> bool {
        self.terminal.get()
    }
}

impl Drop for Retained {
    fn drop(&mut self) {
        let unwinding = std::thread::panicking();
        let note = if unwinding {
            "panicked"
        } else if self.completed.get() {
            "completed"
        } else {
            "dropped-without-outcome"
        };
        self.record_best_effort("unwind", note);
        if !self.terminal.get() {
            self.record_best_effort("outcome", note);
        }
    }
}
