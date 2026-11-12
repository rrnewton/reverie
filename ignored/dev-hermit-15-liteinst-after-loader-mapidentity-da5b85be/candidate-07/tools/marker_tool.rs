use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;

use reverie_ptrace::LiteinstCallerImage;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn main() -> io::Result<()> {
    let mut args = env::args_os().skip(1);
    let mode = args.next().ok_or_else(|| invalid("missing emit|validate"))?;
    let runtime = PathBuf::from(args.next().ok_or_else(|| invalid("missing runtime"))?);
    let marker = PathBuf::from(args.next().ok_or_else(|| invalid("missing marker"))?);
    if args.next().is_some() {
        return Err(invalid("unexpected extra argument"));
    }

    let runtime_bytes = fs::read(&runtime)?;
    let canonical = LiteinstCallerImage::runtime_stage_marker(&runtime_bytes)?;
    match mode.to_str() {
        Some("emit") => {
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&marker)?;
            output.write_all(&canonical)?;
            output.sync_all()?;
        }
        Some("validate") => {}
        _ => return Err(invalid("mode must be emit or validate")),
    }

    if fs::read(&marker)? != canonical {
        return Err(io::Error::other("marker bytes are not canonical"));
    }
    let bound = LiteinstCallerImage::read_runtime(&runtime, &marker)?;
    if bound.runtime_marker() != Some(canonical.as_slice()) {
        return Err(io::Error::other("bound marker differs from producer bytes"));
    }
    Ok(())
}
