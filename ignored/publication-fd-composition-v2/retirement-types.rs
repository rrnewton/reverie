/// Retired descriptions belong to one executor. A scope declared before its
/// file-table/transaction guards keeps closes outside both guards, including
/// ordinary early returns. The queue mutex is never held during destruction.
#[derive(Clone, Default)]
pub(crate) struct FileRetirement(Arc<std::sync::Mutex<FileRetirementState>>);

#[cfg(test)]
type RetirementProbe = Arc<dyn Fn(&[i32]) + Send + Sync>;

#[derive(Default)]
struct FileRetirementState {
    scopes: usize,
    files: Vec<RetiredFile>,
    #[cfg(test)]
    probe: Option<RetirementProbe>,
    #[cfg(test)]
    clones_before_failure: Option<usize>,
}

enum RetiredFile {
    Owned(File),
    Shared(Arc<File>),
}

impl std::fmt::Debug for FileRetirement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileRetirement").finish_non_exhaustive()
    }
}

pub(crate) struct FileRetirementScope(FileRetirement);

pub(crate) struct StagedFile {
    file: Option<File>,
    retirement: FileRetirement,
}

impl FileRetirement {
    pub(crate) fn hold(&self) -> FileRetirementScope {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).scopes += 1;
        FileRetirementScope(self.clone())
    }

    pub(crate) fn retire(&self, files: impl IntoIterator<Item = File>) {
        self.retire_inner(files.into_iter().map(RetiredFile::Owned));
    }

    pub(crate) fn retire_shared(&self, files: impl IntoIterator<Item = Arc<File>>) {
        self.retire_inner(files.into_iter().map(RetiredFile::Shared));
    }

    fn retire_inner(&self, files: impl IntoIterator<Item = RetiredFile>) {
        let retired = {
            let mut state = self.0.lock().unwrap_or_else(|p| p.into_inner());
            state.files.extend(files);
            if state.scopes == 0 {
                std::mem::take(&mut state.files)
            } else {
                Vec::new()
            }
        };
        self.destroy(retired);
    }

    /// The caller has released both executor guards and is about to enter an
    /// operation that may block. Do not retain stale descriptions across it.
    pub(crate) fn drain_unlocked(&self) {
        let retired = {
            let mut state = self.0.lock().unwrap_or_else(|p| p.into_inner());
            std::mem::take(&mut state.files)
        };
        self.destroy(retired);
    }

    fn destroy(&self, files: Vec<RetiredFile>) {
        if files.is_empty() {
            return;
        }
        #[cfg(test)]
        {
            let probe = self.0.lock().unwrap_or_else(|p| p.into_inner()).probe.clone();
            if let Some(probe) = probe {
                let descriptors: Vec<_> = files
                    .iter()
                    .map(|file| match file {
                        RetiredFile::Owned(file) => file.as_raw_fd(),
                        RetiredFile::Shared(file) => file.as_raw_fd(),
                    })
                    .collect();
                // The actual owners remain alive until the probe permits this
                // destructor to continue; no queue or executor guard is held.
                probe(&descriptors);
            }
        }
        for file in files {
            match file {
                RetiredFile::Owned(file) => drop(file),
                RetiredFile::Shared(file) => drop(file),
            }
        }
    }

    pub(crate) fn stage(&self, file: File) -> StagedFile {
        StagedFile {
            file: Some(file),
            retirement: self.clone(),
        }
    }

    pub(crate) fn stage_clone(&self, file: &File) -> std::io::Result<StagedFile> {
        #[cfg(test)]
        {
            let mut state = self.0.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(remaining) = state.clones_before_failure.as_mut() {
                if *remaining == 0 {
                    return Err(std::io::Error::from_raw_os_error(libc::EMFILE));
                }
                *remaining -= 1;
            }
        }
        file.try_clone().map(|file| self.stage(file))
    }

    #[cfg(test)]
    pub(crate) fn set_probe(&self, probe: Option<RetirementProbe>) {
        let retired = {
            let mut state = self.0.lock().unwrap_or_else(|p| p.into_inner());
            std::mem::replace(&mut state.probe, probe)
        };
        drop(retired);
    }

    #[cfg(test)]
    pub(crate) fn fail_clone_after(&self, successful: Option<usize>) {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).clones_before_failure = successful;
    }
}

impl Drop for FileRetirementScope {
    fn drop(&mut self) {
        let retired = {
            let mut state = self.0.0.lock().unwrap_or_else(|p| p.into_inner());
            state.scopes -= 1;
            if state.scopes == 0 {
                std::mem::take(&mut state.files)
            } else {
                Vec::new()
            }
        };
        self.0.destroy(retired);
    }
}

impl StagedFile {
    pub(crate) fn as_file(&self) -> &File {
        self.file.as_ref().expect("staged descriptor was already transferred")
    }

    pub(crate) fn into_file(mut self) -> File {
        self.file.take().expect("staged descriptor was already transferred")
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        self.retirement.retire(self.file.take());
    }
}

