//! Closed private-static source dispatch. Construction is restricted to the
//! checked native action factory; these types are never serialized authority.
use std::cell::RefCell;

use super::super::native_action::Custody;
use super::super::native_build::DirectoryGuard;
use super::super::native_source::NativeSourceBinding;
use super::*;

macro_rules! source_roles {
    ($(($variant:ident, $key:literal, $path:literal, $producer:literal, $compiled:literal)),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub(super) enum StaticSourceRole { $($variant),+ }
        impl StaticSourceRole {
            fn key(self) -> &'static str { match self { $(Self::$variant => $key),+ } }
            fn path(self) -> &'static str { match self { $(Self::$variant => $path),+ } }
            fn from_path(path: &str) -> Result<Self> {
                match path { $($path => Ok(Self::$variant),)+ _ => Err("unknown private-static source role".into()) }
            }
        }
        pub(in super::super) fn native_source_roles() -> &'static [(&'static str, &'static str, bool)] {
            &[$(($key, $producer, $compiled)),+]
        }
    };
}
source_roles! {
    (Entry, "workspace:detcore-liteinst/src/exec_initial/mapped_entry.rs", "detcore-liteinst/src/exec_initial/mapped_entry.rs", "detcore-liteinst", true),
    (InguestManifest, "rv:reverie-inguest/Cargo.toml", "reverie-inguest/Cargo.toml", "reverie-inguest", false),
    (InguestLib, "rv:reverie-inguest/src/lib.rs", "reverie-inguest/src/lib.rs", "reverie-inguest", true),
    (Private, "rv:reverie-inguest/src/private_runtime/mod.rs", "reverie-inguest/src/private_runtime/mod.rs", "reverie-inguest", true),
    (Tls, "rv:reverie-inguest/src/private_runtime/tls/mod.rs", "reverie-inguest/src/private_runtime/tls/mod.rs", "reverie-inguest", true),
    (TlsAbi, "rv:reverie-inguest/src/private_runtime/tls/abi.rs", "reverie-inguest/src/private_runtime/tls/abi.rs", "reverie-inguest", true),
    (TlsLayout, "rv:reverie-inguest/src/private_runtime/tls/layout.rs", "reverie-inguest/src/private_runtime/tls/layout.rs", "reverie-inguest", true),
    (LoaderManifest, "rv:reverie-loader/Cargo.toml", "reverie-loader/Cargo.toml", "reverie-loader", false),
    (LoaderLib, "rv:reverie-loader/src/lib.rs", "reverie-loader/src/lib.rs", "reverie-loader", true),
    (MuslTls, "rv:reverie-loader/src/musl_tls.rs", "reverie-loader/src/musl_tls.rs", "reverie-loader", true),
    (MappedInputs, "rv:reverie-loader/src/mapped_inputs.rs", "reverie-loader/src/mapped_inputs.rs", "reverie-loader", true),
    (Handoff, "rv:reverie-loader/src/handoff.rs", "reverie-loader/src/handoff.rs", "reverie-loader", true),
    (ExecPrivate, "rv:reverie-inguest/src/guest/exec_private.rs", "reverie-inguest/src/guest/exec_private.rs", "reverie-inguest", true),
    (Mapped, "rv:reverie-inguest/src/guest/exec_private/mapped_initial.rs", "reverie-inguest/src/guest/exec_private/mapped_initial.rs", "reverie-inguest", true),
    (Scalar, "rv:reverie-inguest/src/guest/exec_private/mapped_initial/mapped_scalar.rs", "reverie-inguest/src/guest/exec_private/mapped_initial/mapped_scalar.rs", "reverie-inguest", true),
    (Relay, "rv:reverie-inguest/src/guest/exec_private/mapped_initial/libc_relay.rs", "reverie-inguest/src/guest/exec_private/mapped_initial/libc_relay.rs", "reverie-inguest", true),
    (Frontend, "rv:reverie-inguest/src/guest/exec_private/mapped_initial/libc_relay/frontend.rs", "reverie-inguest/src/guest/exec_private/mapped_initial/libc_relay/frontend.rs", "reverie-inguest", true),
    (Trap, "rv:reverie-inguest/src/trap.rs", "reverie-inguest/src/trap.rs", "reverie-inguest", true),
    (Frame, "rv:reverie-inguest/src/trap/frame.rs", "reverie-inguest/src/trap/frame.rs", "reverie-inguest", true),
    (Gate, "rv:reverie-inguest/src/trap/fixed_gates.rs", "reverie-inguest/src/trap/fixed_gates.rs", "reverie-inguest", true),
}

/// Borrowed native archive source custody, distinct from Cargo and DSO scopes.
/// Only the checked staticlib factory can create it. Publication must wait for
/// that factory's final source/archive/custody close after the callback returns.
pub struct NativeStaticSource<'a> {
    binding: &'a NativeSourceBinding,
    custody: &'a Custody,
    evidence: &'a FileIdentity,
    fingerprint: String,
    archive_owner: Arc<ArchiveSourceOwner>,
    output: PathBuf,
    output_parent: DirectoryGuard,
    output_guard: RefCell<Option<DirectoryGuard>>,
    output_created: std::cell::Cell<bool>,
    // Identities acquired by later link/inspection phases remain owned until
    // the enclosing callback closes, including partially failed phases.
    retained_files: RefCell<Vec<FileIdentity>>,
    command_ledgers: RefCell<Vec<(CommandObservations, usize)>>,
    environment: BTreeMap<std::ffi::OsString, std::ffi::OsString>,
}

/// An actual checked normal staticlib product, never a constructor/entry token.
/// Its private allocation binds it to one still-live native source transaction.
pub struct NativeStaticArchive {
    archive_owner: Arc<ArchiveSourceOwner>,
    file: FileIdentity,
    evidence: FileIdentity,
    fingerprint: String,
}
impl NativeStaticArchive {
    pub fn file(&self) -> &FileIdentity {
        &self.file
    }
    pub fn evidence(&self) -> &FileIdentity {
        &self.evidence
    }
    pub fn source_sha256(&self) -> &str {
        &self.fingerprint
    }
    pub fn verify(&self) -> Result<()> {
        let errors: Vec<_> = [self.file.verify(), self.evidence.verify()]
            .into_iter()
            .filter_map(|result| result.err())
            .collect();
        require(errors.is_empty(), errors.join("; "))
    }
}
impl NativeStaticSource<'_> {
    /// Recheck this live source, archive, evidence and output namespace before
    /// a later inspection phase. The enclosing factory still closes afterward.
    pub fn verify_unchanged(&self) -> Result<()> {
        self.close()
    }
    pub fn source_sha256(&self) -> &str {
        &self.fingerprint
    }
    fn register_runner(&self, runner: &Runner) -> Result<()> {
        let mut ledgers = self.command_ledgers.borrow_mut();
        if !ledgers
            .iter()
            .any(|(ledger, _)| Arc::ptr_eq(ledger, &runner.command_observations))
        {
            let from = runner
                .command_observations
                .lock()
                .map_err(|_| "command observation ledger poisoned")?
                .len();
            ledgers.push((Arc::clone(&runner.command_observations), from));
        }
        Ok(())
    }
    fn retain_commands(&self, runner: &Runner, from: usize) -> Result<()> {
        let ledger = runner
            .command_observations
            .lock()
            .map_err(|_| "command observation ledger poisoned")?;
        let observations = ledger
            .get(from..)
            .ok_or("native static command observation offset differs")?;
        // Retain originals before checking the public rows: an altered report
        // must not suppress the independent stream-file close obligations.
        StaticSourceRef::Native(self).retain_files(observations.iter().filter_map(|observation| {
            match observation {
                CommandObservation::Stream(file) => Some(file),
                CommandObservation::Command(_, _) => None,
            }
        }));
        require(
            observations.iter().all(|observation| match observation {
                CommandObservation::Stream(_) => true,
                CommandObservation::Command(index, command) => {
                    runner.commands.get(*index) == Some(command)
                }
            }),
            "native static command report was erased or replaced",
        )
    }
    fn close(&self) -> Result<()> {
        let mut errors = Vec::new();
        for file in self
            .binding
            .files
            .values()
            .chain([&self.binding.artifact, self.evidence])
        {
            if let Err(error) = file.verify() {
                errors.push(error);
            }
        }
        // These Arc-owned original observations survive replacement/drop of
        // any Runner used by the callback or by a physical helper.
        for (ledger, from) in self.command_ledgers.borrow().iter() {
            let observations = ledger.lock();
            match observations {
                Ok(observations) => {
                    for observation in &observations[*from..] {
                        if let CommandObservation::Stream(file) = observation {
                            match file.verify() {
                                Ok(()) => {}
                                Err(error) => errors.push(error),
                            }
                        }
                    }
                }
                Err(_) => errors.push("command observation ledger poisoned".into()),
            }
        }
        for file in self.retained_files.borrow().iter() {
            if let Err(error) = file.verify() {
                errors.push(error);
            }
        }
        if let Err(error) = self.custody.close() {
            errors.push(error);
        }
        if std::env::vars_os().collect::<BTreeMap<_, _>>() != self.environment {
            errors.push("native static environment changed during transaction".into());
        }
        if let Err(error) = self.output_parent.verify() {
            errors.push(error);
        }
        if let Some(guard) = self.output_guard.borrow().as_ref() {
            if let Err(error) = guard.verify() {
                errors.push(error);
            }
        } else if self.output_created.get() {
            errors
                .push("native static output was created without a completed identity guard".into());
        } else {
            match fs::symlink_metadata(&self.output) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Ok(_) => {
                    errors.push("native static output appeared outside the owned linker".into())
                }
                Err(error) => errors.push(format!("native static output close: {error}")),
            }
        }
        require(errors.is_empty(), errors.join("; "))
    }
}

#[derive(Clone, Copy)]
pub(super) enum StaticSourceRef<'a> {
    Cargo(&'a VerifiedStaticSource),
    Native(&'a NativeStaticSource<'a>),
}
#[derive(Clone, Copy)]
pub(super) enum StaticArchiveRef<'a> {
    Cargo(&'a StaticArchive),
    Native(&'a NativeStaticArchive),
}
impl StaticArchiveRef<'_> {
    pub(super) fn file(&self) -> &FileIdentity {
        match self {
            Self::Cargo(archive) => &archive.file,
            Self::Native(archive) => &archive.file,
        }
    }
    fn owner(&self) -> &Arc<ArchiveSourceOwner> {
        match self {
            Self::Cargo(archive) => &archive.archive_owner,
            Self::Native(archive) => &archive.archive_owner,
        }
    }
    pub(super) fn verify(&self) -> Result<()> {
        match self {
            Self::Cargo(archive) => archive.verify(),
            Self::Native(archive) => archive.verify(),
        }
    }
}
impl StaticSourceRef<'_> {
    pub(super) fn register_runner(&self, runner: &Runner) -> Result<()> {
        match self {
            Self::Cargo(_) => Ok(()),
            Self::Native(source) => source.register_runner(runner),
        }
    }
    pub(super) fn retain_files<'a>(&self, files: impl IntoIterator<Item = &'a FileIdentity>) {
        if let Self::Native(source) = self {
            let mut retained = source.retained_files.borrow_mut();
            for file in files {
                // Keep conflicting identities too: a later observation must
                // never overwrite the original close obligation.
                if !retained.contains(file) {
                    retained.push(file.clone());
                }
            }
        }
    }
    pub(super) fn with_native_memcpy<T>(
        &self,
        inputs: &super::super::private_machine_diagnostic::NativeMemcpyInputs<'_>,
        artifact: &FileIdentity,
        body: impl FnOnce(&super::super::private_machine_diagnostic::NativeMemcpyRole) -> Result<T>,
    ) -> Result<T> {
        if matches!(self, Self::Cargo(_)) {
            return super::super::private_machine_diagnostic::with_native_memcpy(
                inputs, artifact, body,
            );
        }
        let (result, files) = super::super::private_machine_diagnostic::with_native_memcpy_files(
            inputs, artifact, body,
        );
        self.retain_files(&files);
        result
    }
    pub(super) fn inspect_memcpy_entry(
        &self,
        inputs: &super::super::private_machine_diagnostic::NativeMemcpyInputs<'_>,
        artifact: &FileIdentity,
        objdump: &FileIdentity,
        runner: &mut Runner,
        ordinary: Option<&selected_initializers::OrdinaryGateObservations>,
    ) -> Result<super::super::private_machine_diagnostic::EntryTransferDiagnostic> {
        if matches!(self, Self::Cargo(_)) {
            return match ordinary {
                Some(ordinary) => super::super::private_machine_diagnostic::inspect_entry_transfers_with_ordinary_and_memcpy(ordinary, inputs, artifact, objdump, runner),
                None => super::super::private_machine_diagnostic::inspect_entry_transfers_with_memcpy(inputs, artifact, objdump, runner),
            };
        }
        self.register_runner(runner)?;
        let (result, files) =
            super::super::private_machine_diagnostic::inspect_entry_transfers_with_memcpy_files(
                inputs, artifact, objdump, runner, ordinary,
            );
        self.retain_files(&files);
        result
    }
    pub(super) fn owner(&self) -> &Arc<ArchiveSourceOwner> {
        match self {
            Self::Cargo(source) => &source.archive_owner,
            Self::Native(source) => &source.archive_owner,
        }
    }
    pub(super) fn close(&self, runner: &mut Runner) -> Result<()> {
        match self {
            Self::Cargo(source) => source.guard.close(runner),
            Self::Native(source) => source.close(),
        }
    }
    pub(super) fn require_archive_owner(&self, archive: StaticArchiveRef<'_>) -> Result<()> {
        // Allocation comparison precedes variant/provenance checks and ALL
        // archive or foreign-proof reads. Equal bytes cannot replace this.
        require(
            Arc::ptr_eq(self.owner(), archive.owner()),
            "static archive belongs to another source transaction",
        )?;
        match (self, archive) {
            (Self::Cargo(source), StaticArchiveRef::Cargo(archive)) => {
                source.require_archive_owner(archive)
            }
            (Self::Native(source), StaticArchiveRef::Native(archive)) => require(
                source.fingerprint == archive.fingerprint,
                "native static archive differs from its source transaction",
            ),
            _ => Err("static archive/source provenance kinds differ".into()),
        }
    }
    pub(super) fn cargo_runtime_root(source: &VerifiedStaticSource) -> Result<&Path> {
        let roots: Vec<_> = source
            .guard
            .roots
            .iter()
            .filter(|(_, kind)| matches!(kind, RootBinding::Cargo(_)))
            .map(|(path, _)| path.as_path())
            .collect();
        require(
            roots.len() == 1,
            "static resolved runtime source root ambiguous",
        )?;
        Ok(roots[0])
    }
    fn source_file_with_bound(
        &self,
        relative: &str,
        files: &mut Vec<FileIdentity>,
        bound: Option<u64>,
    ) -> Result<Option<FileIdentity>> {
        let role = StaticSourceRole::from_path(relative)?;
        let optional = matches!(
            role,
            StaticSourceRole::MuslTls | StaticSourceRole::MappedInputs
        );
        let path = match self {
            Self::Cargo(source) => {
                let root = if role == StaticSourceRole::Entry {
                    source.workspace.as_path()
                } else {
                    Self::cargo_runtime_root(source)?
                };
                root.join(role.path())
            }
            Self::Native(source) => source
                .binding
                .files
                .get(role.key())
                .ok_or_else(|| format!("native static source role missing: {}", role.key()))?
                .path
                .clone(),
        };
        // Historical non-TLS Cargo readers canonicalize through FileIdentity;
        // keep that path behavior unchanged. Cargo TLS calls the original bind.
        if matches!(self, Self::Cargo(_)) && bound.is_none() {
            let file = FileIdentity::read(&path)?;
            files.push(file.clone());
            return Ok(Some(file));
        }
        match fs::symlink_metadata(&path) {
            Err(error)
                if optional
                    && matches!(self, Self::Cargo(_))
                    && error.kind() == std::io::ErrorKind::NotFound =>
            {
                return Ok(None);
            }
            Err(error) => {
                return Err(format!(
                    "static source role metadata {}: {error}",
                    path.display()
                ));
            }
            Ok(metadata) => require(
                metadata.file_type().is_file()
                    && bound.is_none_or(|maximum| metadata.len() <= maximum),
                "static source role is not a bounded regular file",
            )?,
        }
        let file = FileIdentity::read(&path)?;
        files.push(file.clone());
        if let Self::Native(source) = self {
            require(
                source.binding.files.get(role.key()) == Some(&file),
                "native static source role changed after action binding",
            )?;
        }
        Ok(Some(file))
    }
    pub(super) fn tls_source_file(
        &self,
        relative: &str,
        files: &mut Vec<FileIdentity>,
    ) -> Result<Option<FileIdentity>> {
        self.source_file_with_bound(relative, files, Some(64 * 1024))
    }
    pub(super) fn source_file(
        &self,
        relative: &str,
        files: &mut Vec<FileIdentity>,
    ) -> Result<FileIdentity> {
        self.source_file_with_bound(relative, files, None)?
            .ok_or_else(|| format!("required static source role absent: {relative}"))
    }
    pub(super) fn terminal_source_file(
        &self,
        relative: &str,
        cargo: bool,
        files: &mut Vec<FileIdentity>,
    ) -> Result<FileIdentity> {
        if let Self::Cargo(source) = self {
            let roots: Vec<_> = source
                .guard
                .roots
                .iter()
                .filter(|(path, kind)| {
                    matches!(kind, RootBinding::Cargo(_)) == cargo && path.join(relative).is_file()
                })
                .map(|(path, _)| path)
                .collect();
            require(roots.len() == 1, "terminal guarded source root ambiguous")?;
            let file = FileIdentity::read(&roots[0].join(relative))?;
            files.push(file.clone());
            return Ok(file);
        }
        let role = StaticSourceRole::from_path(relative)?;
        require(
            (role != StaticSourceRole::Entry) == cargo,
            "terminal source role origin differs",
        )?;
        self.source_file(relative, files)
    }
    pub(super) fn create_link_output(&self, output: &Path) -> Result<()> {
        match self {
            Self::Cargo(source) => {
                require(
                    output.is_absolute()
                        && output.starts_with(&source.target_dir)
                        && output.parent().is_some_and(|parent| {
                            fs::canonicalize(parent).ok().as_deref() == Some(parent)
                        }),
                    "initializer link output must have canonical sole-target parent",
                )?;
                fs::create_dir(output).map_err(|error| error.to_string())
            }
            Self::Native(source) => {
                require(
                    output == source.output && !source.output_created.get(),
                    "native static link output differs or was reused",
                )?;
                source.output_parent.verify()?;
                fs::create_dir(output)
                    .map_err(|error| format!("create native static output: {error}"))?;
                source.output_created.set(true);
                *source.output_guard.borrow_mut() = Some(DirectoryGuard::capture(output)?);
                Ok(())
            }
        }
    }
}

/// Internal entry only: the caller must have completed actual staticlib owner,
/// role/source/profile/recipe/inventory/output joins and kept custody live.
/// Both optional TLS roles are required for the initial native shared-inputs
/// cohort; no omission from a caller map authorizes an older TLS tuple.
pub(in super::super) fn with_checked_native_static<T>(
    binding: &NativeSourceBinding,
    custody: &Custody,
    evidence: &FileIdentity,
    link_output_directory: &Path,
    runner: &mut Runner,
    consume: impl FnOnce(&NativeStaticSource<'_>, &NativeStaticArchive, &mut Runner) -> Result<T>,
) -> Result<T> {
    let result = (|| {
        for (key, _, compiled) in native_source_roles() {
            require(
                binding.files.contains_key(*key),
                format!("native static source role missing: {key}"),
            )?;
            if *compiled {
                require(
                    binding.compiler_filenames.contains_key(*key),
                    format!("native static compiled member missing: {key}"),
                )?;
            }
        }
        require(
            link_output_directory.is_absolute()
                && link_output_directory.components().all(|part| {
                    matches!(
                        part,
                        std::path::Component::RootDir | std::path::Component::Normal(_)
                    )
                }),
            "native static output needs an absolute ordinary path",
        )?;
        let parent = link_output_directory
            .parent()
            .ok_or("native static output lacks parent")?;
        require(
            fs::canonicalize(parent).map_err(|error| error.to_string())? == parent,
            "native static output parent is not canonical",
        )?;
        match fs::symlink_metadata(link_output_directory) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => return Err("native static output already exists".into()),
            Err(error) => return Err(format!("native static output metadata: {error}")),
        }
        let output_parent = DirectoryGuard::capture(parent)?;
        let fingerprint =
            sha256(&serde_json::to_vec(&(binding, evidence)).map_err(|error| error.to_string())?);
        let owner = Arc::new(ArchiveSourceOwner { _reserved: 0 });
        let source = NativeStaticSource {
            binding,
            custody,
            evidence,
            fingerprint: fingerprint.clone(),
            archive_owner: Arc::clone(&owner),
            output: link_output_directory.to_owned(),
            output_parent,
            output_guard: RefCell::new(None),
            output_created: std::cell::Cell::new(false),
            retained_files: RefCell::new(Vec::new()),
            command_ledgers: RefCell::new(Vec::new()),
            environment: std::env::vars_os().collect(),
        };
        let archive = NativeStaticArchive {
            archive_owner: owner,
            file: binding.artifact.clone(),
            evidence: evidence.clone(),
            fingerprint,
        };
        let mut files = Vec::new();
        source.register_runner(runner)?;
        let original_commands = Arc::clone(&runner.command_observations);
        let command_from = original_commands
            .lock()
            .map_err(|_| "command observation ledger poisoned")?
            .len();
        let admitted = (|| {
            source.close()?;
            archive.verify()?;
            // Exactly the historical whole-file TLS tuples and geometry.
            tls_sources::bind_source(StaticSourceRef::Native(&source), &mut files)?;
            consume(&source, &archive, runner)
        })();
        let mut errors = Vec::new();
        if !Arc::ptr_eq(&original_commands, &runner.command_observations) {
            errors.push("native static runner was replaced during callback".into());
        }
        if let Err(error) = source.retain_commands(runner, command_from) {
            errors.push(error);
        }
        if let Err(error) = source.close() {
            errors.push(error);
        }
        if let Err(error) = archive.verify() {
            errors.push(error);
        }
        for file in files {
            if let Err(error) = file.verify() {
                errors.push(error);
            }
        }
        close_result(admitted, require(errors.is_empty(), errors.join("; ")))
    })();
    // Admission failures before constructing the source still close every
    // opened native action input; primary failures never mask this close.
    close_result(result, custody.close())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU64;
    use std::sync::atomic::Ordering;

    use super::*;
    static NEXT: AtomicU64 = AtomicU64::new(0);

    // Private synthetic custody fixtures only. They do not execute a compiler,
    // satisfy the reviewed TLS source tuples, or qualify an archive/ELF.
    struct Fixture {
        root: PathBuf,
        binding: NativeSourceBinding,
        evidence: FileIdentity,
        custody: Custody,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "static-native-source-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            let root = fs::canonicalize(root).unwrap();
            for (name, bytes) in [
                ("source.rs", b"source".as_slice()),
                ("archive.a", b"!<arch>\n".as_slice()),
                ("evidence.json", b"synthetic observation".as_slice()),
            ] {
                fs::write(root.join(name), bytes).unwrap();
            }
            fs::create_dir(root.join("output-parent")).unwrap();
            Self {
                binding: NativeSourceBinding {
                    files: BTreeMap::from([(
                        "rv:reverie-loader/src/handoff.rs".into(),
                        FileIdentity::read(&root.join("source.rs")).unwrap(),
                    )]),
                    compiler_filenames: BTreeMap::new(),
                    packages: BTreeMap::new(),
                    artifact: FileIdentity::read(&root.join("archive.a")).unwrap(),
                },
                evidence: FileIdentity::read(&root.join("evidence.json")).unwrap(),
                custody: Custody::new(),
                root,
            }
        }
        fn source(&self) -> NativeStaticSource<'_> {
            NativeStaticSource {
                binding: &self.binding,
                custody: &self.custody,
                evidence: &self.evidence,
                fingerprint: "same-observation-is-not-allocation-identity".into(),
                archive_owner: Arc::new(ArchiveSourceOwner { _reserved: 0 }),
                output: self.root.join("output-parent/linked"),
                output_parent: DirectoryGuard::capture(&self.root.join("output-parent")).unwrap(),
                output_guard: RefCell::new(None),
                output_created: std::cell::Cell::new(false),
                retained_files: RefCell::new(Vec::new()),
                command_ledgers: RefCell::new(Vec::new()),
                environment: std::env::vars_os().collect(),
            }
        }
        fn archive(&self, source: &NativeStaticSource<'_>) -> NativeStaticArchive {
            NativeStaticArchive {
                archive_owner: Arc::clone(&source.archive_owner),
                file: self.binding.artifact.clone(),
                evidence: self.evidence.clone(),
                fingerprint: source.fingerprint.clone(),
            }
        }
        fn runner(&self) -> Runner {
            Runner::new(self.root.join("logs"), Duration::from_secs(5), 1024).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn private_static_roles_are_closed_and_manifests_are_not_compiler_members() {
        let roles = native_source_roles();
        assert_eq!(roles.len(), 20);
        assert_eq!(
            roles.iter().map(|row| row.0).collect::<BTreeSet<_>>().len(),
            20
        );
        assert_eq!(
            roles
                .iter()
                .filter(|row| !row.2)
                .map(|row| row.0)
                .collect::<Vec<_>>(),
            vec![
                "rv:reverie-inguest/Cargo.toml",
                "rv:reverie-loader/Cargo.toml"
            ]
        );
        assert_eq!(
            roles[0],
            (
                "workspace:detcore-liteinst/src/exec_initial/mapped_entry.rs",
                "detcore-liteinst",
                true
            )
        );
        assert!(StaticSourceRole::from_path("../reverie-loader/src/handoff.rs").is_err());
        assert!(StaticSourceRole::from_path("unlisted.rs").is_err());
    }

    #[test]
    fn native_static_shared_view_uses_checked_role_and_same_live_owner() {
        let fixture = Fixture::new();
        let source = fixture.source();
        let archive = fixture.archive(&source);
        let view = StaticSourceRef::Native(&source);
        view.require_archive_owner(StaticArchiveRef::Native(&archive))
            .unwrap();
        let mut files = Vec::new();
        assert_eq!(
            view.source_file("reverie-loader/src/handoff.rs", &mut files)
                .unwrap(),
            fixture.binding.files["rv:reverie-loader/src/handoff.rs"]
        );
        assert_eq!(files.len(), 1);
        assert!(
            view.source_file("reverie-inguest/src/trap.rs", &mut files)
                .is_err()
        );
        source.verify_unchanged().unwrap();
        archive.verify().unwrap();
    }

    #[test]
    fn foreign_static_archive_refuses_before_link_or_foreign_file_reads() {
        let fixture = Fixture::new();
        let first = fixture.source();
        let second = fixture.source();
        let mut archive = fixture.archive(&first);
        assert_eq!(first.fingerprint, second.fingerprint);
        archive.file.path = fixture.root.join("absent-foreign-archive");
        archive.evidence.path = fixture.root.join("absent-foreign-evidence");
        let missing = fixture.root.join("absent-tool-and-proof");
        let input = selected_initializers::InitializerLinkInputs {
            lld: &missing,
            script: &missing,
            libc: &missing,
            supporting_archives: &[],
            memcpy_source: &missing,
            musl_source_authority: &missing,
            stack_chk_source: &missing,
            output_directory: &second.output,
        };
        let mut runner = fixture.runner();
        let error =
            match second.link_and_join_initializer_definitions(&archive, &input, &mut runner) {
                Ok(_) => panic!("foreign archive reached source/link admission"),
                Err(error) => error,
            };
        assert_eq!(
            error,
            "static archive belongs to another source transaction"
        );
        assert!(runner.commands.is_empty());
        assert!(!second.output.exists());
        let native = selected_initializers::NativeLeafInputs {
            memcpy: super::super::super::private_machine_diagnostic::NativeMemcpyInputs {
                archive: &missing,
                source: &missing,
                source_authority: &missing,
                link_map: &missing,
            },
            memset_source: &missing,
            set_thread_area_source: &missing,
        };
        let foreign_file = FileIdentity {
            path: missing.clone(),
            sha256: "absent".into(),
        };
        let error = second
            .with_native_leaf_definitions(
                &archive,
                &native,
                &foreign_file,
                &foreign_file,
                &mut runner,
                |_| Ok(()),
            )
            .unwrap_err();
        assert_eq!(
            error,
            "static archive belongs to another source transaction"
        );
        assert!(runner.commands.is_empty());
    }

    #[test]
    fn native_static_output_is_exact_fresh_and_guarded_through_close() {
        let fixture = Fixture::new();
        let source = fixture.source();
        let view = StaticSourceRef::Native(&source);
        assert!(
            view.create_link_output(&fixture.root.join("different"))
                .is_err()
        );
        assert!(!source.output.exists());
        view.create_link_output(&source.output).unwrap();
        source.verify_unchanged().unwrap();
        assert!(view.create_link_output(&source.output).is_err());
        fs::rename(&source.output, fixture.root.join("moved-output")).unwrap();
        fs::create_dir(&source.output).unwrap();
        assert!(source.verify_unchanged().is_err());
    }

    #[test]
    fn native_static_close_preserves_primary_source_evidence_and_parent_failures() {
        let fixture = Fixture::new();
        let source = fixture.source();
        fs::write(fixture.root.join("source.rs"), b"changed source").unwrap();
        fs::write(&fixture.evidence.path, b"changed evidence").unwrap();
        fs::rename(
            fixture.root.join("output-parent"),
            fixture.root.join("moved-parent"),
        )
        .unwrap();
        fs::create_dir(fixture.root.join("output-parent")).unwrap();
        let error = close_result(
            Err::<(), _>("primary inspection failure".into()),
            source.verify_unchanged(),
        )
        .unwrap_err();
        for expected in [
            "primary inspection failure",
            "source.rs",
            "evidence.json",
            "owned directory",
        ] {
            assert!(error.contains(expected), "missing {expected}: {error}");
        }
    }

    #[test]
    fn native_static_role_records_cannot_skip_missing_or_wrong_tls_tuple() {
        let mut fixture = Fixture::new();
        let file = fixture.binding.files["rv:reverie-loader/src/handoff.rs"].clone();
        for (key, _, compiled) in native_source_roles() {
            fixture.binding.files.insert((*key).into(), file.clone());
            if *compiled {
                fixture
                    .binding
                    .compiler_filenames
                    .insert((*key).into(), b"synthetic/source.rs".to_vec());
            }
        }
        let mut runner = fixture.runner();
        let called = std::cell::Cell::new(false);
        for optional in [
            "rv:reverie-loader/src/musl_tls.rs",
            "rv:reverie-loader/src/mapped_inputs.rs",
        ] {
            let removed = fixture.binding.files.remove(optional).unwrap();
            let error = with_checked_native_static(
                &fixture.binding,
                &fixture.custody,
                &fixture.evidence,
                &fixture.root.join("output-parent/linked"),
                &mut runner,
                |_, _, _| {
                    called.set(true);
                    Ok(())
                },
            )
            .unwrap_err();
            assert!(error.contains(optional), "{error}");
            fixture.binding.files.insert(optional.into(), removed);
        }
        let error = with_checked_native_static(
            &fixture.binding,
            &fixture.custody,
            &fixture.evidence,
            &fixture.root.join("output-parent/linked"),
            &mut runner,
            |_, _, _| {
                called.set(true);
                Ok(())
            },
        )
        .unwrap_err();
        assert!(error.contains("not one complete reviewed tuple"), "{error}");
        assert!(!called.get());
        assert!(runner.commands.is_empty());
        assert!(!fixture.root.join("output-parent/linked").exists());
    }

    #[test]
    fn native_static_outer_close_retains_phase_files_without_replacing_prior_identities() {
        let fixture = Fixture::new();
        let source = fixture.source();
        let view = StaticSourceRef::Native(&source);
        view.create_link_output(&source.output).unwrap();
        // File-custody observations only, never a valid ELF/map/link fixture.
        let paths = [
            source.output.join("observation.not-an-elf"),
            source.output.join("observation.not-a-map"),
            fixture.root.join("link-input"),
        ];
        let files: Vec<_> = paths
            .iter()
            .map(|path| {
                fs::write(path, b"first observation").unwrap();
                FileIdentity::read(path).unwrap()
            })
            .collect();
        view.retain_files(&files);
        source.verify_unchanged().unwrap();
        for path in &paths {
            fs::write(path, b"changed after helper returned").unwrap();
        }
        // Re-observing a changed file cannot overwrite its prior obligation.
        let changed: Vec<_> = paths
            .iter()
            .map(|path| FileIdentity::read(path).unwrap())
            .collect();
        view.retain_files(&changed);
        let error = close_result(
            Err::<(), _>("primary callback refusal".into()),
            source.verify_unchanged(),
        )
        .unwrap_err();
        assert!(error.contains("primary callback refusal"), "{error}");
        for path in &paths {
            assert!(error.contains(path.to_str().unwrap()), "{error}");
        }
    }

    #[test]
    fn native_static_outer_close_retains_partially_refused_nested_memcpy_inputs() {
        let fixture = Fixture::new();
        let source = fixture.source();
        let wrong_libc = fixture.root.join("wrong-libc.a");
        fs::write(&wrong_libc, b"deliberately not the pinned libc").unwrap();
        let missing = fixture.root.join("unread-later-input");
        let inputs = super::super::super::private_machine_diagnostic::NativeMemcpyInputs {
            archive: &wrong_libc,
            source: &missing,
            source_authority: &missing,
            link_map: &missing,
        };
        let called = std::cell::Cell::new(false);
        let error = StaticSourceRef::Native(&source)
            .with_native_memcpy(&inputs, &fixture.binding.artifact, |_| {
                called.set(true);
                Ok(())
            })
            .unwrap_err();
        assert!(
            error.contains("native input expected identity differs"),
            "{error}"
        );
        assert!(!called.get());
        source.verify_unchanged().unwrap();
        fs::write(&wrong_libc, b"changed after nested refusal").unwrap();
        let error = close_result(Err::<(), _>(error), source.verify_unchanged()).unwrap_err();
        assert!(
            error.contains("native input expected identity differs"),
            "{error}"
        );
        assert!(error.contains(wrong_libc.to_str().unwrap()), "{error}");
    }

    #[test]
    fn native_static_outer_close_retains_original_runner_stream_identities() {
        let fixture = Fixture::new();
        let source = fixture.source();
        let mut runner = fixture.runner();
        let from = runner.command_observations.lock().unwrap().len();
        let output = runner
            .run(
                "/bin/sh",
                &[
                    "-c".into(),
                    "printf phase-output; printf phase-error >&2".into(),
                ],
                &fixture.root,
            )
            .unwrap();
        assert_eq!(output, b"phase-output");
        source.retain_commands(&runner, from).unwrap();
        source.verify_unchanged().unwrap();
        let command = &runner.commands[from];
        fs::write(&command.stdout.path, b"changed later stdout").unwrap();
        fs::write(&command.stderr.path, b"changed later stderr").unwrap();
        let error = source.verify_unchanged().unwrap_err();
        assert!(
            error.contains(command.stdout.path.to_str().unwrap()),
            "{error}"
        );
        assert!(
            error.contains(command.stderr.path.to_str().unwrap()),
            "{error}"
        );
        let invalid_offset = runner.command_observations.lock().unwrap().len() + 1;
        assert!(source.retain_commands(&runner, invalid_offset).is_err());
    }

    #[test]
    fn native_static_outer_close_refuses_erased_or_replaced_command_reports() {
        for replace in [false, true] {
            let fixture = Fixture::new();
            let source = fixture.source();
            let mut runner = fixture.runner();
            let from = runner.command_observations.lock().unwrap().len();
            let primary = runner
                .run(
                    "/bin/sh",
                    &[
                        "-c".into(),
                        "printf original; printf original-error >&2; exit 7".into(),
                    ],
                    &fixture.root,
                )
                .unwrap_err();
            assert!(primary.contains("failed"), "{primary}");
            let original = runner.commands[0].clone();
            fs::write(&original.stdout.path, b"changed stdout after helper return").unwrap();
            fs::write(&original.stderr.path, b"changed stderr after helper return").unwrap();
            if replace {
                runner.commands[0].stdout = FileIdentity::read(&original.stdout.path).unwrap();
                runner.commands[0].stderr = FileIdentity::read(&original.stderr.path).unwrap();
            } else {
                runner.commands.clear();
            }
            let report_close = source.retain_commands(&runner, from);
            let close = close_result(report_close, source.verify_unchanged());
            let error = close_result(Err::<(), _>(primary), close).unwrap_err();
            assert!(error.contains("failed"), "{error}");
            assert!(
                error.contains("command report was erased or replaced"),
                "{error}"
            );
            assert!(
                error.contains(original.stdout.path.to_str().unwrap()),
                "{error}"
            );
            assert!(
                error.contains(original.stderr.path.to_str().unwrap()),
                "{error}"
            );
        }
    }

    #[test]
    fn native_static_outer_close_retains_alternate_runner_after_its_value_is_dropped() {
        let fixture = Fixture::new();
        let source = fixture.source();
        let default_runner = fixture.runner();
        source.register_runner(&default_runner).unwrap();
        let mut helper_runner = Runner::new(
            fixture.root.join("helper-logs"),
            Duration::from_secs(5),
            1024,
        )
        .unwrap();
        // The same closed dispatcher registration used before every physical
        // helper run, including helpers supplied a different Runner.
        StaticSourceRef::Native(&source)
            .register_runner(&helper_runner)
            .unwrap();
        helper_runner
            .run(
                "/bin/sh",
                &["-c".into(), "printf helper; printf helper-error >&2".into()],
                &fixture.root,
            )
            .unwrap();
        let original = helper_runner.commands[0].clone();
        drop(helper_runner);
        source.verify_unchanged().unwrap();
        fs::write(
            &original.stdout.path,
            b"changed after alternate runner dropped",
        )
        .unwrap();
        fs::write(
            &original.stderr.path,
            b"changed after alternate runner dropped",
        )
        .unwrap();
        let error = source.verify_unchanged().unwrap_err();
        assert!(
            error.contains(original.stdout.path.to_str().unwrap()),
            "{error}"
        );
        assert!(
            error.contains(original.stderr.path.to_str().unwrap()),
            "{error}"
        );
        assert!(default_runner.commands.is_empty());
    }

    #[test]
    fn native_static_outer_close_retains_stdout_when_stderr_identity_read_fails() {
        let fixture = Fixture::new();
        let source = fixture.source();
        let runner = fixture.runner();
        source.register_runner(&runner).unwrap();
        let stdout = runner.logs.join("partial.stdout");
        let stderr = runner.logs.join("missing.stderr");
        fs::write(&stdout, b"original partial stream").unwrap();
        // Exercise the actual Runner acquisition helper in its normal order.
        runner.observe_stream(&stdout).unwrap();
        let primary = runner.observe_stream(&stderr).unwrap_err();
        assert!(primary.contains(stderr.to_str().unwrap()), "{primary}");
        assert!(runner.commands.is_empty());
        source.verify_unchanged().unwrap();
        fs::write(&stdout, b"changed after second stream admission failed").unwrap();
        drop(runner);
        let error = close_result(Err::<(), _>(primary), source.verify_unchanged()).unwrap_err();
        assert!(error.contains(stderr.to_str().unwrap()), "{error}");
        assert!(error.contains(stdout.to_str().unwrap()), "{error}");
    }
}
