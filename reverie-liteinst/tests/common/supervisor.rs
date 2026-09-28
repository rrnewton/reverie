use std::process::Command;
use std::process::Output;
use std::process::Stdio;
use std::thread;
use std::time::Duration;
use std::time::Instant;

pub struct Report {
    pub output: Output,
    pub events: Vec<[u64; 10]>,
    pub counter_results: Vec<CounterResult>,
    pub counter_unavailable: Vec<CounterUnavailable>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CounterResult {
    pub mode: String,
    pub fd: u64,
    pub event_id: u64,
    pub owner: u64,
    pub cpu: u64,
    pub clock: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CounterUnavailable {
    pub mode: String,
    pub errno: i32,
}

fn take_counter_results(stderr: &mut Vec<u8>) -> (Vec<CounterResult>, Vec<CounterUnavailable>) {
    const PREFIX: &str = "liteinst hardware counter: ";
    let mut retained = Vec::with_capacity(stderr.len());
    let mut results = Vec::new();
    let mut unavailable = Vec::new();
    for line in stderr.split_inclusive(|byte| *byte == b'\n') {
        let bare = line.strip_suffix(b"\n").unwrap_or(line);
        let Some(text) = std::str::from_utf8(bare)
            .ok()
            .and_then(|text| text.strip_prefix(PREFIX))
        else {
            retained.extend_from_slice(line);
            continue;
        };
        let mut fields = std::collections::BTreeMap::new();
        for field in text.split_whitespace() {
            let (key, value) = field.split_once('=').expect("hardware counter field");
            assert!(fields.insert(key, value).is_none(), "duplicate {key}");
        }
        let mode = fields.remove("mode").expect("hardware counter mode").to_owned();
        if let Some(errno) = fields.remove("unavailable-errno") {
            assert!(fields.is_empty(), "unexpected unavailable fields: {fields:?}");
            unavailable.push(CounterUnavailable {
                mode,
                errno: errno.parse().expect("hardware counter errno"),
            });
            continue;
        }
        let mut number = |name: &str| {
            fields
                .remove(name)
                .unwrap_or_else(|| panic!("missing hardware counter {name}"))
                .parse::<u64>()
                .unwrap_or_else(|_| panic!("invalid hardware counter {name}"))
        };
        let result = CounterResult {
            mode,
            fd: number("fd"),
            event_id: number("event-id"),
            owner: number("owner"),
            cpu: number("cpu"),
            clock: number("clock"),
        };
        drop(number);
        assert!(fields.is_empty(), "unexpected hardware counter fields: {fields:?}");
        results.push(result);
    }
    *stderr = retained;
    (results, unavailable)
}

pub fn run(command: Command, timeout: Duration, output_limit: Option<u64>) -> Report {
    run_inner(command, timeout, output_limit, true)
}

#[cfg(feature = "rcb-qualification")]
pub fn run_profile_refusal(
    command: Command,
    timeout: Duration,
    output_limit: Option<u64>,
) -> Report {
    run_inner(command, timeout, output_limit, false)
}

fn run_inner(
    command: Command,
    timeout: Duration,
    output_limit: Option<u64>,
    require_complete_service: bool,
) -> Report {
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::CommandExt;
    use std::os::unix::process::ExitStatusExt;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;
    let mode = command.get_args().next().unwrap();
    // These existing fixtures exit natively before installing a Tool when
    // OSPKE is absent. Execute their actual exit/output predicates unchanged;
    // there is no authenticated acquisition in that existing refusal branch.
    let ospke_refusal = ["owned-frame", "memory-access", "syscall-fallback-pkey"]
        .iter()
        .any(|name| mode == *name)
        && core::arch::x86_64::__cpuid_count(7, 0).ecx & (1 << 4) == 0;
    let direct = mode == "syscall-fallback-refusal" || ospke_refusal;
    #[cfg(feature = "rcb-qualification")]
    let direct = direct || mode == "rcb-inherited-sqpoll";
    let (mut status_read, status_write) = UnixStream::pair().unwrap();
    status_read
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let status_fd = status_write.as_raw_fd();
    let mut launcher = Command::new(command.get_program());
    if !direct {
        launcher.arg("supervise").arg(status_fd.to_string());
    }
    launcher.args(command.get_args());
    for (key, value) in command.get_envs() {
        match value {
            Some(value) => {
                launcher.env(key, value);
            }
            None => {
                launcher.env_remove(key);
            }
        }
    }
    if let Some(directory) = command.get_current_dir() {
        launcher.current_dir(directory);
    }
    launcher
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    unsafe {
        launcher.pre_exec(move || {
            if libc::setsid() == -1 || (!direct && libc::fcntl(status_fd, libc::F_SETFD, 0) == -1) {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = launcher.spawn().unwrap();
    drop(status_write);
    let exceeded = Arc::new(AtomicBool::new(false));
    let read = |mut pipe: Box<dyn Read + Send>, exceeded: Arc<AtomicBool>| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let mut block = [0_u8; 4096];
            loop {
                let count = pipe.read(&mut block).unwrap();
                if count == 0 {
                    break;
                }
                bytes.extend_from_slice(&block[..count]);
                if output_limit.is_some_and(|limit| bytes.len() as u64 > limit) {
                    exceeded.store(true, Ordering::Release);
                    break;
                }
            }
            bytes
        })
    };
    let stdout = read(Box::new(child.stdout.take().unwrap()), exceeded.clone());
    let stderr = read(Box::new(child.stderr.take().unwrap()), exceeded.clone());
    let deadline = Instant::now() + timeout;
    let mut bounded = false;
    loop {
        if stdout.is_finished() && stderr.is_finished() && child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline || exceeded.load(Ordering::Acquire) {
            bounded = true;
            // The unreaped live launcher owns this newly created process group.
            // Kill its real descendants too, before joining their output pipes.
            assert_eq!(
                unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) },
                0
            );
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    // A successful try_wait already cached the same terminal status; otherwise
    // this reaps after the owned group was signalled. No group signal follows.
    let supervisor_status = child.wait().unwrap();
    let mut output = Output {
        status: supervisor_status,
        stdout: stdout.join().unwrap(),
        stderr: stderr.join().unwrap(),
    };
    let (counter_results, counter_unavailable) = take_counter_results(&mut output.stderr);
    assert!(
        !bounded && !exceeded.load(Ordering::Acquire),
        "fixture exceeded {timeout:?}/{output_limit:?}: {output:?}"
    );
    if ospke_refusal {
        assert_eq!(output.status.code(), Some(77), "{output:?}");
        eprintln!(
            "native OSPKE refusal: actual status=77 setup=not completed acquisition=unmeasured"
        );
    }
    let mut events = Vec::new();
    if !direct {
        let mut raw = [0_u8; 16];
        status_read
            .read_exact(&mut raw)
            .expect("actual guest wait record");
        let words: [u32; 4] = std::array::from_fn(|index| {
            u32::from_le_bytes(raw[index * 4..index * 4 + 4].try_into().unwrap())
        });
        assert_eq!(words[0], 0x3157_494c);
        output.status = std::process::ExitStatus::from_raw(words[1] as i32);
        let mut count = [0_u8; 4];
        status_read.read_exact(&mut count).unwrap();
        let count = u32::from_le_bytes(count);
        assert!(count <= 64);
        for _ in 0..count {
            let mut event = [0_u8; 80];
            status_read.read_exact(&mut event).unwrap();
            let event: [u64; 10] = std::array::from_fn(|index| {
                u64::from_le_bytes(event[index * 8..index * 8 + 8].try_into().unwrap())
            });
            assert_eq!(event[0], 1);
            assert_ne!(event[5], 0);
            assert!(event[8] <= i32::MAX as u64, "explicit target CPU");
            assert_eq!(event[9], 0, "event was acquired physically disabled");
            eprintln!("actual acquisition event: {event:?}");
            events.push(event);
        }
        if require_complete_service {
            assert!(
                supervisor_status.success(),
                "supervisor={supervisor_status} guest={output:?} setup/service={:?}",
                &words[2..]
            );
            assert_eq!(
                &words[2..],
                &[1, 1],
                "setup and supervision must both complete"
            );
        } else {
            assert_eq!(supervisor_status.code(), Some(120), "{supervisor_status}");
            assert_eq!(output.status.code(), Some(126), "{output:?}");
            assert_ne!(
                &words[2..],
                &[1, 1],
                "corrupt supervisor profile unexpectedly completed"
            );
        }
    }
    Report {
        output,
        events,
        counter_results,
        counter_unavailable,
    }
}
