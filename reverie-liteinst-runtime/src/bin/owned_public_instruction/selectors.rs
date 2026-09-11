//! Pre-spawn refusal of constructor, runtime and loader selection.
//!
//! The default-feature binary links the `preload-constructor` `.init_array` entry
//! `REVERIE_LITEINST_INIT` (`src/lib.rs`), which calls `reverie_liteinst_initialize`
//! and then `runtime::initialize_from_environment`. That function selects the host
//! runtime on `REVERIE_LITEINST_HOST_RUNTIME=1`, or a tool / shared built-in on
//! `REVERIE_LITEINST_TOOL`, and otherwise returns `Ok(())` without installing
//! anything. So the constructor exists and runs; what the fixture relies on is its
//! audited *unselected* early return.
//!
//! This guard makes that reliance checkable instead of assumed. It **refuses** a
//! conflicting environment; it never unsets or edits one, and it never disables the
//! default feature. Refusing keeps the caller's environment observable to the gate
//! preflight that binds the absence.
//!
//! It is a pure function over name/value pairs so refusal can be tested with
//! synthetic input, without spawning an instrumented binary and without mutating
//! the parent environment.

/// Variables this harness owns. They carry no product meaning and are allowed.
pub const OWNED_PREFIX: &str = "OWNED_PUBLIC_INSTRUCTION_";

/// Why one variable was refused.
#[derive(Debug, Eq, PartialEq)]
pub struct Refusal {
    pub name: String,
    pub reason: &'static str,
}

/// Refuse product selection and loader interposition, in a stable order.
///
/// A name is refused when it selects LiteInst behaviour (`REVERIE_`, `LITEINST_`)
/// or interposes on the loader (`LD_`, `GLIBC_TUNABLES`). The harness's own
/// `OWNED_PUBLIC_INSTRUCTION_` names are exempt because nothing reads them before
/// `main`.
pub fn refusals<I, S>(variables: I) -> Vec<Refusal>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut refused: Vec<Refusal> = variables
        .into_iter()
        .filter_map(|name| {
            let name = name.as_ref();
            if name.starts_with(OWNED_PREFIX) {
                return None;
            }
            let reason = if name.starts_with("REVERIE_") || name.starts_with("LITEINST_") {
                "selects LiteInst constructor or runtime behaviour before main"
            } else if name.starts_with("LD_") || name == "GLIBC_TUNABLES" {
                "interposes on the dynamic loader"
            } else {
                return None;
            };
            Some(Refusal {
                name: name.to_owned(),
                reason,
            })
        })
        .collect();
    refused.sort_by(|left, right| left.name.cmp(&right.name));
    refused.dedup_by(|left, right| left.name == right.name);
    refused
}

/// Render refusals as a stable, retainable report. `None` means nothing refused.
pub fn report(refused: &[Refusal]) -> Option<String> {
    if refused.is_empty() {
        return None;
    }
    let mut text = String::from("refusing conflicting selection before spawn:\n");
    for entry in refused {
        text.push_str("  ");
        text.push_str(&entry.name);
        text.push_str(": ");
        text.push_str(entry.reason);
        text.push('\n');
    }
    Some(text)
}

/// The current process environment as plain names, lossily decoded.
pub fn current_names() -> Vec<String> {
    std::env::vars_os()
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .collect()
}
