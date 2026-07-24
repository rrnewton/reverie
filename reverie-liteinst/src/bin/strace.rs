use std::env;
use std::ffi::OsStr;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::Command;
use std::process::{self};

fn main() {
    let mut arguments = env::args_os();
    let _launcher = arguments.next();
    let Some(program) = arguments.next() else {
        eprintln!("usage: reverie-liteinst-strace PROGRAM [ARG]...");
        process::exit(2);
    };

    let preload = match preload_path() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("reverie-liteinst-strace: {error}");
            process::exit(2);
        }
    };

    let mut preload_value = preload.into_os_string();
    if let Some(existing) = env::var_os("LD_PRELOAD").filter(|value| !value.is_empty()) {
        preload_value.push(OsStr::new(":"));
        preload_value.push(existing);
    }

    let status = match Command::new(program)
        .args(arguments)
        .env("LD_PRELOAD", preload_value)
        .env("REVERIE_LITEINST_TOOL", "strace")
        .status()
    {
        Ok(status) => status,
        Err(error) => {
            eprintln!("reverie-liteinst-strace: failed to launch guest: {error}");
            process::exit(1);
        }
    };

    if let Some(code) = status.code() {
        process::exit(code);
    }
    if let Some(signal) = status.signal() {
        eprintln!("reverie-liteinst-strace: guest terminated by signal {signal}");
        process::exit(128 + signal);
    }
    process::exit(1);
}

fn preload_path() -> Result<PathBuf, String> {
    if let Some(path) = env::var_os("REVERIE_LITEINST_PRELOAD") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!(
            "REVERIE_LITEINST_PRELOAD does not name a file: {}",
            path.display()
        ));
    }

    let executable =
        env::current_exe().map_err(|error| format!("cannot locate launcher: {error}"))?;
    let parent = executable
        .parent()
        .ok_or_else(|| "launcher has no parent directory".to_owned())?;
    let candidates = [
        parent.join("libreverie_liteinst.so"),
        parent.join("deps/libreverie_liteinst.so"),
        parent
            .parent()
            .unwrap_or(parent)
            .join("libreverie_liteinst.so"),
    ];
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| {
            format!(
                "cannot find libreverie_liteinst.so beside {}",
                executable.display()
            )
        })
}
