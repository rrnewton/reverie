#!/usr/bin/env -S rust-script --force
//! Serve one generated compatibility-website tree on the IPv4 loopback interface.
//!
//! ```cargo
//! [dependencies]
//! libc = "0.2"
//! ```

#![deny(
    dead_code,
    unused_imports,
    unused_mut,
    unused_variables,
    unused_must_use
)]

#[path = "../../hermit/scripts/lib/rust_script_prelude.rs"]
mod rust_script_prelude;

use std::collections::BTreeMap;
use std::env;
use std::ffi::{CString, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_PORT: u16 = 0;
const MAX_REQUEST_BYTES: usize = 16 * 1024;
const REQUEST_DEADLINE: Duration = Duration::from_secs(1);
const RESPONSE_DEADLINE: Duration = Duration::from_secs(30);
const WRITE_WAIT: Duration = Duration::from_millis(100);
const ACCEPT_POLL: Duration = Duration::from_millis(20);
const STREAM_BUFFER_BYTES: usize = 16 * 1024;
const SERVER_NAME: &str = "compatibility-website serve";

const USAGE: &str = r#"Usage: ci-hub/compatibility-website/serve.rs --root DIRECTORY [--port PORT]

Serve a generated compatibility website on 127.0.0.1. The default port is 0,
which asks the operating system for an available port. The command prints one
ready URL on stdout and then serves until SIGINT or SIGTERM.

Options:
  --root DIRECTORY  Existing, nonsymlink website output directory (required).
  --port PORT       Loopback TCP port, from 0 through 65535 (default: 0).
  -h, --help        Show this help.
"#;

const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; font-src 'none'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'";

static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

struct Config {
    root: PathBuf,
    port: u16,
}

enum CliAction {
    Help,
    Serve(Config),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Method {
    Get,
    Head,
}

struct Request {
    method: Method,
    components: Vec<Vec<u8>>,
}

#[derive(Clone, Copy)]
enum Status {
    BadRequest,
    NotFound,
    MethodNotAllowed,
    HttpVersionNotSupported,
}

impl Status {
    fn code_and_reason(self) -> (u16, &'static str) {
        match self {
            Self::BadRequest => (400, "Bad Request"),
            Self::NotFound => (404, "Not Found"),
            Self::MethodNotAllowed => (405, "Method Not Allowed"),
            Self::HttpVersionNotSupported => (505, "HTTP Version Not Supported"),
        }
    }
}

struct RequestFailure {
    status: Status,
    head_only: bool,
}

impl RequestFailure {
    fn new(status: Status, head_only: bool) -> Self {
        Self { status, head_only }
    }
}

#[derive(Clone, Copy)]
struct MediaType {
    content_type: &'static str,
    content_encoding: Option<&'static str>,
}

struct OpenFile {
    file: File,
    length: u64,
    media_type: MediaType,
}

fn main() -> ExitCode {
    rust_script_prelude::init();
    match parse_arguments(env::args_os().skip(1)) {
        Ok(CliAction::Help) => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(CliAction::Serve(config)) => match serve(config) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{SERVER_NAME}: {error}");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("{SERVER_NAME}: {error}\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn parse_arguments(arguments: impl Iterator<Item = OsString>) -> Result<CliAction, String> {
    let mut root = None;
    let mut port = None;
    let mut arguments = arguments.peekable();
    while let Some(argument) = arguments.next() {
        let option = argument
            .to_str()
            .ok_or_else(|| "option names must be valid UTF-8".to_owned())?;
        match option {
            "-h" | "--help" => {
                if root.is_some() || port.is_some() || arguments.peek().is_some() {
                    return Err("--help cannot be combined with other arguments".to_owned());
                }
                return Ok(CliAction::Help);
            }
            "--root" => {
                if root.is_some() {
                    return Err("--root may be supplied only once".to_owned());
                }
                root = Some(
                    arguments
                        .next()
                        .ok_or_else(|| "--root requires a directory".to_owned())?
                        .into(),
                );
            }
            "--port" => {
                if port.is_some() {
                    return Err("--port may be supplied only once".to_owned());
                }
                let value = arguments
                    .next()
                    .ok_or_else(|| "--port requires a number".to_owned())?;
                let value = value
                    .to_str()
                    .ok_or_else(|| "--port must be valid UTF-8".to_owned())?;
                port =
                    Some(value.parse::<u16>().map_err(|_| {
                        "--port must be an integer from 0 through 65535".to_owned()
                    })?);
            }
            _ => return Err(format!("unexpected argument {option:?}")),
        }
    }
    let root = root.ok_or_else(|| "--root is required".to_owned())?;
    Ok(CliAction::Serve(Config {
        root,
        port: port.unwrap_or(DEFAULT_PORT),
    }))
}

fn serve(config: Config) -> Result<(), String> {
    let root = open_root(&config.root)?;
    install_stop_handlers()?;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, config.port))
        .map_err(|error| format!("cannot bind 127.0.0.1:{}: {error}", config.port))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("cannot make listener nonblocking: {error}"))?;
    let address = listener
        .local_addr()
        .map_err(|error| format!("cannot read listener address: {error}"))?;
    println!("http://127.0.0.1:{}/", address.port());
    io::stdout()
        .flush()
        .map_err(|error| format!("cannot publish ready URL: {error}"))?;

    while !STOP_REQUESTED.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _peer)) => handle_connection(stream, &root, address.port()),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(format!("listener failed: {error}")),
        }
    }
    Ok(())
}

fn open_root(path: &Path) -> Result<OwnedFd, String> {
    if path.as_os_str().is_empty() {
        return Err("website root is empty".to_owned());
    }
    let anchor = if path.is_absolute() { b"/" } else { b"." };
    let encoded = CString::new(anchor.as_slice())
        .map_err(|_| "internal website-root anchor contains a NUL byte".to_owned())?;
    let flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW;
    let anchor_descriptor = unsafe { libc::open(encoded.as_ptr(), flags) };
    if anchor_descriptor < 0 {
        return Err(format!(
            "cannot open website root {:?}: {}",
            path,
            io::Error::last_os_error()
        ));
    }
    let mut descriptor = unsafe { OwnedFd::from_raw_fd(anchor_descriptor) };
    for component in path.components() {
        let name = match component {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(name) => name,
            Component::ParentDir | Component::Prefix(_) => {
                return Err(format!("website root {:?} is not a direct path", path));
            }
        };
        let encoded = CString::new(name.as_bytes())
            .map_err(|_| "website root contains a NUL byte".to_owned())?;
        let next = unsafe { libc::openat(descriptor.as_raw_fd(), encoded.as_ptr(), flags) };
        if next < 0 {
            return Err(format!(
                "cannot open website root {:?}: {}",
                path,
                io::Error::last_os_error()
            ));
        }
        descriptor = unsafe { OwnedFd::from_raw_fd(next) };
    }
    if !descriptor_is_directory(descriptor.as_raw_fd()) {
        return Err(format!("website root {:?} is not a directory", path));
    }
    Ok(descriptor)
}

fn descriptor_is_directory(descriptor: i32) -> bool {
    file_stat(descriptor)
        .map(|stat| stat.st_mode & libc::S_IFMT == libc::S_IFDIR)
        .unwrap_or(false)
}

fn file_stat(descriptor: i32) -> io::Result<libc::stat> {
    let mut stat = unsafe { std::mem::zeroed::<libc::stat>() };
    if unsafe { libc::fstat(descriptor, &raw mut stat) } == 0 {
        Ok(stat)
    } else {
        Err(io::Error::last_os_error())
    }
}

fn install_stop_handlers() -> Result<(), String> {
    unsafe extern "C" fn request_stop(_signal: libc::c_int) {
        STOP_REQUESTED.store(true, Ordering::Relaxed);
    }

    for signal in [libc::SIGINT, libc::SIGTERM] {
        let previous =
            unsafe { libc::signal(signal, request_stop as *const () as libc::sighandler_t) };
        if previous == libc::SIG_ERR {
            return Err(format!(
                "cannot install signal handler for signal {signal}: {}",
                io::Error::last_os_error()
            ));
        }
    }
    Ok(())
}

fn handle_connection(mut stream: TcpStream, root: &OwnedFd, port: u16) {
    let request = match read_request(&mut stream, port) {
        Ok(request) => request,
        Err(failure) => {
            let _ignored = write_error(&mut stream, failure.status, failure.head_only);
            return;
        }
    };
    let media_type = match media_type(&request.components) {
        Some(media_type) => media_type,
        None => {
            let _ignored = write_error(
                &mut stream,
                Status::NotFound,
                request.method == Method::Head,
            );
            return;
        }
    };
    let opened = match open_regular_file(root, &request.components, media_type) {
        Some(opened) => opened,
        None => {
            let _ignored = write_error(
                &mut stream,
                Status::NotFound,
                request.method == Method::Head,
            );
            return;
        }
    };
    let _ignored = write_file_response(&mut stream, request.method, opened);
}

fn read_request(stream: &mut TcpStream, port: u16) -> Result<Request, RequestFailure> {
    let request_bytes = read_header_block(stream)?;
    parse_request(&request_bytes, port)
}

fn read_header_block(stream: &mut TcpStream) -> Result<Vec<u8>, RequestFailure> {
    let deadline = Instant::now() + REQUEST_DEADLINE;
    let mut request = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 1024];
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| RequestFailure::new(Status::BadRequest, false))?;
        stream
            .set_read_timeout(Some(remaining))
            .map_err(|_| RequestFailure::new(Status::BadRequest, false))?;
        let count = match stream.read(&mut chunk) {
            Ok(0) => return Err(RequestFailure::new(Status::BadRequest, false)),
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(RequestFailure::new(Status::BadRequest, false)),
        };
        if request.len() + count > MAX_REQUEST_BYTES {
            return Err(RequestFailure::new(Status::BadRequest, false));
        }
        request.extend_from_slice(&chunk[..count]);
        if let Some(end) = find_header_end(&request) {
            if end != request.len() {
                return Err(RequestFailure::new(Status::BadRequest, false));
            }
            return Ok(request);
        }
    }
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

fn parse_request(bytes: &[u8], port: u16) -> Result<Request, RequestFailure> {
    if !has_only_crlf_line_endings(bytes) || bytes.iter().any(|byte| !valid_request_byte(*byte)) {
        return Err(RequestFailure::new(Status::BadRequest, false));
    }
    let text =
        std::str::from_utf8(bytes).map_err(|_| RequestFailure::new(Status::BadRequest, false))?;
    let mut lines = text[..text.len() - 4].split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| RequestFailure::new(Status::BadRequest, false))?;
    let (method_text, target, version) = parse_request_line(request_line)?;
    let head_only = method_text == "HEAD";
    let headers = parse_headers(lines, head_only)?;
    validate_version(version, head_only)?;
    validate_host(&headers, version, port, head_only)?;
    if headers.contains_key("content-length")
        || headers.contains_key("transfer-encoding")
        || headers.contains_key("expect")
    {
        return Err(RequestFailure::new(Status::BadRequest, head_only));
    }
    let method = match method_text {
        "GET" => Method::Get,
        "HEAD" => Method::Head,
        _ => return Err(RequestFailure::new(Status::MethodNotAllowed, false)),
    };
    let components = parse_target(target)
        .map_err(|()| RequestFailure::new(Status::NotFound, method == Method::Head))?;
    Ok(Request { method, components })
}

fn has_only_crlf_line_endings(bytes: &[u8]) -> bool {
    bytes.iter().enumerate().all(|(index, byte)| match byte {
        b'\r' => bytes.get(index + 1) == Some(&b'\n'),
        b'\n' => index > 0 && bytes[index - 1] == b'\r',
        _ => true,
    })
}

fn valid_request_byte(byte: u8) -> bool {
    byte == b'\r' || byte == b'\n' || (b' '..=b'~').contains(&byte)
}

fn parse_request_line(line: &str) -> Result<(&str, &str, &str), RequestFailure> {
    let fields = line.split(' ').collect::<Vec<_>>();
    if fields.len() != 3 || fields.iter().any(|field| field.is_empty()) {
        return Err(RequestFailure::new(Status::BadRequest, false));
    }
    Ok((fields[0], fields[1], fields[2]))
}

fn parse_headers<'a>(
    lines: impl Iterator<Item = &'a str>,
    head_only: bool,
) -> Result<BTreeMap<String, String>, RequestFailure> {
    let mut headers = BTreeMap::new();
    for line in lines {
        if line.is_empty() || line.starts_with([' ', '\t']) {
            return Err(RequestFailure::new(Status::BadRequest, head_only));
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| RequestFailure::new(Status::BadRequest, head_only))?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(RequestFailure::new(Status::BadRequest, head_only));
        }
        let name = name.to_ascii_lowercase();
        let value = value.strip_prefix(' ').unwrap_or(value);
        if value.is_empty()
            || value.starts_with(' ')
            || value.ends_with(' ')
            || headers.insert(name, value.to_owned()).is_some()
        {
            return Err(RequestFailure::new(Status::BadRequest, head_only));
        }
    }
    Ok(headers)
}

fn validate_version(version: &str, head_only: bool) -> Result<(), RequestFailure> {
    match version {
        "HTTP/1.0" | "HTTP/1.1" => Ok(()),
        value if value.starts_with("HTTP/") => Err(RequestFailure::new(
            Status::HttpVersionNotSupported,
            head_only,
        )),
        _ => Err(RequestFailure::new(Status::BadRequest, head_only)),
    }
}

fn validate_host(
    headers: &BTreeMap<String, String>,
    version: &str,
    port: u16,
    head_only: bool,
) -> Result<(), RequestFailure> {
    let expected = format!("127.0.0.1:{port}");
    match headers.get("host") {
        Some(host) if host == &expected || (port == 80 && host == "127.0.0.1") => Ok(()),
        None if version == "HTTP/1.0" => Ok(()),
        _ => Err(RequestFailure::new(Status::BadRequest, head_only)),
    }
}

fn parse_target(target: &str) -> Result<Vec<Vec<u8>>, ()> {
    if target.contains('#') {
        return Err(());
    }
    let path = target.split_once('?').map_or(target, |(path, _query)| path);
    if !path.starts_with('/') {
        return Err(());
    }
    let decoded = percent_decode_path(path.as_bytes())?;
    if decoded == b"/" {
        return Ok(vec![b"index.html".to_vec()]);
    }
    let mut components = Vec::new();
    for component in decoded[1..].split(|byte| *byte == b'/') {
        if component.is_empty()
            || component == b"."
            || component == b".."
            || component.contains(&b'%')
        {
            return Err(());
        }
        components.push(component.to_vec());
    }
    if components.is_empty() {
        return Err(());
    }
    Ok(components)
}

fn percent_decode_path(path: &[u8]) -> Result<Vec<u8>, ()> {
    let mut decoded = Vec::with_capacity(path.len());
    let mut index = 0;
    while index < path.len() {
        let byte = path[index];
        if byte == b'%' {
            let high = *path.get(index + 1).ok_or(())?;
            let low = *path.get(index + 2).ok_or(())?;
            let value = decode_hex(high)? * 16 + decode_hex(low)?;
            if value == b'/' || value == b'\\' {
                return Err(());
            }
            decoded.push(value);
            index += 3;
            continue;
        }
        decoded.push(byte);
        index += 1;
    }
    if decoded.iter().any(|byte| {
        *byte == 0 || *byte == b'\\' || *byte < b' ' || *byte == 0x7f || !byte.is_ascii()
    }) {
        return Err(());
    }
    Ok(decoded)
}

fn decode_hex(byte: u8) -> Result<u8, ()> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(()),
    }
}

fn media_type(components: &[Vec<u8>]) -> Option<MediaType> {
    let name = components.last()?;
    let parent = (components.len() == 2).then(|| components[0].as_slice());
    if components.len() == 1 && name == b"build.json" {
        Some(MediaType {
            content_type: "application/json",
            content_encoding: None,
        })
    } else if name.ends_with(b".html") {
        Some(MediaType {
            content_type: "text/html; charset=utf-8",
            content_encoding: None,
        })
    } else if name.ends_with(b".css") {
        Some(MediaType {
            content_type: "text/css; charset=utf-8",
            content_encoding: None,
        })
    } else if name.ends_with(b".js") {
        Some(MediaType {
            content_type: "text/javascript; charset=utf-8",
            content_encoding: None,
        })
    } else if parent == Some(b"data") && name.ends_with(b".json.gz") {
        Some(MediaType {
            content_type: "application/json",
            content_encoding: Some("gzip"),
        })
    } else if parent == Some(b"logs") && valid_log_object_name(name) {
        Some(MediaType {
            content_type: "text/plain; charset=utf-8",
            content_encoding: Some("gzip"),
        })
    } else {
        None
    }
}

fn valid_log_object_name(name: &[u8]) -> bool {
    let Some(digest) = name.strip_suffix(b".log.gz") else {
        return false;
    };
    digest.len() == 64
        && digest
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

fn open_regular_file(
    root: &OwnedFd,
    components: &[Vec<u8>],
    media_type: MediaType,
) -> Option<OpenFile> {
    let mut opened_directory = None;
    for (index, component) in components.iter().enumerate() {
        let name = CString::new(component.as_slice()).ok()?;
        let final_component = index + 1 == components.len();
        let flags = if final_component {
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK
        } else {
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY
        };
        let directory = opened_directory
            .as_ref()
            .map_or(root.as_raw_fd(), AsRawFd::as_raw_fd);
        let descriptor = unsafe { libc::openat(directory, name.as_ptr(), flags) };
        if descriptor < 0 {
            return None;
        }
        let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
        if !final_component {
            opened_directory = Some(descriptor);
            continue;
        }
        let stat = file_stat(descriptor.as_raw_fd()).ok()?;
        if stat.st_mode & libc::S_IFMT != libc::S_IFREG || stat.st_size < 0 {
            return None;
        }
        return Some(OpenFile {
            file: descriptor.into(),
            length: stat.st_size as u64,
            media_type,
        });
    }
    None
}

fn write_file_response(
    stream: &mut TcpStream,
    method: Method,
    mut opened: OpenFile,
) -> io::Result<()> {
    let deadline = Instant::now() + RESPONSE_DEADLINE;
    write_headers(
        stream,
        200,
        "OK",
        opened.media_type,
        opened.length,
        None,
        deadline,
    )?;
    if method == Method::Head {
        return Ok(());
    }
    let mut remaining = opened.length;
    let mut buffer = [0_u8; STREAM_BUFFER_BYTES];
    while remaining > 0 {
        let limit = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let count = opened.file.read(&mut buffer[..limit])?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "served file became shorter while reading",
            ));
        }
        write_all_before(stream, &buffer[..count], deadline)?;
        remaining -= count as u64;
    }
    Ok(())
}

fn write_error(stream: &mut TcpStream, status: Status, head_only: bool) -> io::Result<()> {
    let deadline = Instant::now() + RESPONSE_DEADLINE;
    let (code, reason) = status.code_and_reason();
    let body = format!("{reason}\n");
    let allow = matches!(status, Status::MethodNotAllowed).then_some("GET, HEAD");
    write_headers(
        stream,
        code,
        reason,
        MediaType {
            content_type: "text/plain; charset=utf-8",
            content_encoding: None,
        },
        body.len() as u64,
        allow,
        deadline,
    )?;
    if !head_only {
        write_all_before(stream, body.as_bytes(), deadline)?;
    }
    Ok(())
}

fn write_headers(
    stream: &mut TcpStream,
    code: u16,
    reason: &str,
    media_type: MediaType,
    content_length: u64,
    allow: Option<&str>,
    deadline: Instant,
) -> io::Result<()> {
    let mut headers = format!(
        "HTTP/1.1 {code} {reason}\r\n\
         Content-Type: {}\r\n\
         Content-Length: {content_length}\r\n\
         Content-Security-Policy: {CONTENT_SECURITY_POLICY}\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Referrer-Policy: no-referrer\r\n\
         X-Frame-Options: DENY\r\n\
         Cross-Origin-Resource-Policy: same-origin\r\n\
         Cross-Origin-Opener-Policy: same-origin\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n",
        media_type.content_type
    );
    if let Some(encoding) = media_type.content_encoding {
        headers.push_str(&format!("Content-Encoding: {encoding}\r\n"));
    }
    if let Some(allow) = allow {
        headers.push_str(&format!("Allow: {allow}\r\n"));
    }
    headers.push_str("\r\n");
    write_all_before(stream, headers.as_bytes(), deadline)
}

fn write_all_before(stream: &mut TcpStream, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        if STOP_REQUESTED.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "server shutdown requested",
            ));
        }
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "response deadline expired"))?;
        let wait = remaining.min(WRITE_WAIT);
        stream.set_write_timeout(Some(wait))?;
        match stream.write(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "socket accepted no response bytes",
                ));
            }
            Ok(count) => bytes = &bytes[count..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
