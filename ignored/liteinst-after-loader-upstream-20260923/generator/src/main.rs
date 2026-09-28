use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use reverie_ptrace::LiteinstCallerImage;
use sha2::{Digest, Sha256};

fn digest_file(path: &Path) -> Result<String, Box<dyn Error>> {
    Ok(format!("{:x}", Sha256::digest(fs::read(path)?)))
}

fn hex(value: &OsStr) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let bytes = value.as_bytes();
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(DIGITS[usize::from(byte >> 4)] as char);
        encoded.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    encoded
}

struct Inputs {
    loader_cache: PathBuf,
    system_preload: PathBuf,
    executable: PathBuf,
    interpreter: PathBuf,
    runtime: PathBuf,
    marker: PathBuf,
    provider: PathBuf,
    libgcc: PathBuf,
}

fn manifest(
    inputs: &Inputs,
    environment: BTreeMap<&[u8], &[u8]>,
) -> Result<String, Box<dyn Error>> {
    let mut document = format!(
        "schema=3\n\
profile=DynamicX86_64EtExecV2\n\
loader_cache={}\t{}\n\
system_preload={}\tabsent\n\
loader_search=profiled-stable-real-file-alias\n\
executable={}\t{}\n\
interpreter=ld-linux-x86-64.so.2\t{}\t{}\n\
runtime={}\t{}\n\
runtime_marker={}\t{}\n\
provider=libc.so.6\t{}\t{}\n",
        hex(inputs.loader_cache.as_os_str()),
        digest_file(&inputs.loader_cache)?,
        hex(inputs.system_preload.as_os_str()),
        hex(inputs.executable.as_os_str()),
        digest_file(&inputs.executable)?,
        hex(inputs.interpreter.as_os_str()),
        digest_file(&inputs.interpreter)?,
        hex(inputs.runtime.as_os_str()),
        digest_file(&inputs.runtime)?,
        hex(inputs.marker.as_os_str()),
        digest_file(&inputs.marker)?,
        hex(inputs.provider.as_os_str()),
        digest_file(&inputs.provider)?,
    );
    for (key, value) in environment {
        document.push_str(&format!(
            "environment={}\t{}\n",
            hex(OsStr::from_bytes(key)),
            hex(OsStr::from_bytes(value)),
        ));
    }
    document.push_str(&format!(
        "image=libgcc_s.so.1\t{}\t{}\n",
        hex(inputs.libgcc.as_os_str()),
        digest_file(&inputs.libgcc)?,
    ));
    Ok(document)
}

fn canonical(path: impl AsRef<Path>) -> Result<PathBuf, Box<dyn Error>> {
    Ok(path.as_ref().canonicalize()?)
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args_os();
    let _program = arguments.next();
    let stage = canonical(arguments.next().ok_or("missing stage directory")?)?;
    let executable = canonical(arguments.next().ok_or("missing fixture executable")?)?;
    let runtime = canonical(arguments.next().ok_or("missing runtime DSO")?)?;
    if arguments.next().is_some() {
        return Err("unexpected extra argument".into());
    }

    let marker = stage.join("runtime.marker");
    if marker.exists() {
        return Err("runtime marker already exists".into());
    }
    let runtime_bytes = fs::read(&runtime)?;
    fs::write(
        &marker,
        LiteinstCallerImage::runtime_stage_marker(&runtime_bytes)?,
    )?;
    let marker = canonical(marker)?;

    let system_preload = PathBuf::from("/etc/ld.so.preload");
    if system_preload.exists() || canonical("/etc")? != PathBuf::from("/etc") {
        return Err("system preload path is not the reviewed absent path".into());
    }
    let inputs = Inputs {
        loader_cache: canonical("/etc/ld.so.cache")?,
        system_preload,
        executable,
        interpreter: canonical("/usr/lib64/ld-linux-x86-64.so.2")?,
        runtime,
        marker,
        provider: canonical("/usr/lib64/libc.so.6")?,
        libgcc: canonical("/usr/lib64/libgcc_s-11-20240719.so.1")?,
    };

    let four = manifest(
        &inputs,
        BTreeMap::from([
            (b"LITEINST_CALLER_OUTPUT".as_slice(), b"canonical-v1".as_slice()),
            (
                b"LITEINST_CALLER_SENTINEL".as_slice(),
                b"preserved".as_slice(),
            ),
        ]),
    )?;
    let one = manifest(
        &inputs,
        BTreeMap::from([
            (b"LITEINST_CALLER_CALLS".as_slice(), b"1".as_slice()),
            (
                b"LITEINST_CALLER_SENTINEL".as_slice(),
                b"preserved".as_slice(),
            ),
        ]),
    )?;
    let unpatchable = manifest(
        &inputs,
        BTreeMap::from([
            (b"LITEINST_CALLER_CALLS".as_slice(), b"1".as_slice()),
            (
                b"LITEINST_CALLER_SENTINEL".as_slice(),
                b"preserved".as_slice(),
            ),
            (
                b"LITEINST_CALLER_SITE".as_slice(),
                b"unpatchable".as_slice(),
            ),
        ]),
    )?;

    let outputs = [
        ("four-canonical.manifest", four),
        ("one-call.manifest", one),
        ("unpatchable.manifest", unpatchable),
    ];
    for (name, document) in outputs {
        let path = stage.join(name);
        if path.exists() {
            return Err(format!("manifest already exists: {}", path.display()).into());
        }
        fs::write(&path, document.as_bytes())?;
        println!("{}  {}", digest_file(&path)?, path.display());
    }
    println!("{}  {}", digest_file(&inputs.marker)?, inputs.marker.display());
    Ok(())
}
