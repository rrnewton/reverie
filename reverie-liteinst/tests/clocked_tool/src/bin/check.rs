use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Child;
use std::process::Command;
use std::process::Output;
use std::process::Stdio;
use std::time::Duration;
use std::time::Instant;

fn finish(mut child: Child) -> Output {
    let limit = Instant::now() + Duration::from_secs(20);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= limit {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!("bounded fixture child timed out: {output:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

struct Coordinator(Child);
impl Drop for Coordinator {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn main() {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    assert_eq!(arguments.len(), 3, "check DSO GUEST EVIDENCE_DIRECTORY");
    let library = Path::new(&arguments[0]);
    let guest = Path::new(&arguments[1]);
    let evidence = Path::new(&arguments[2]);
    std::fs::create_dir_all(evidence).unwrap();
    let inactive = finish(
        Command::new("/bin/true")
            .env("LD_PRELOAD", library)
            .env_remove("CLOCK_FIXTURE_SOCKET")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    assert!(
        inactive.status.success(),
        "inactive initializer: {inactive:?}"
    );
    assert!(inactive.stdout.is_empty() && inactive.stderr.is_empty());
    let failure = finish(
        Command::new("/bin/true")
            .env("LD_PRELOAD", library)
            .env("CLOCK_FIXTURE_SOCKET", "unused")
            .env("CLOCK_FIXTURE_FAIL", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    assert_eq!(failure.status.code(), Some(127));
    assert!(failure.stdout.is_empty() && failure.stderr.is_empty());
    let socket = std::env::temp_dir().join(format!("liteinst-clock-{}.sock", std::process::id()));
    let server = std::env::current_exe()
        .unwrap()
        .with_file_name("liteinst-clocked-tool-fixture");
    let mut coordinator = Coordinator(Command::new(server).arg(&socket).spawn().unwrap());
    let limit = Instant::now() + Duration::from_secs(5);
    while !socket.exists() {
        assert!(
            coordinator.0.try_wait().unwrap().is_none(),
            "coordinator exited before listening"
        );
        assert!(Instant::now() < limit, "coordinator startup timeout");
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut reference = None;
    if std::env::var_os("CLOCK_FIXTURE_SUD").is_some() {
        let refused = finish(
            Command::new(guest)
                .env("LD_PRELOAD", library)
                .env("CLOCK_FIXTURE_SOCKET", &socket)
                .env("CLOCK_FIXTURE_WORK", "0")
                .env("CLOCK_FIXTURE_INSTRUCTIONS", "1")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        std::fs::write(
            evidence.join("instruction-refusal.status"),
            refused.status.to_string(),
        )
        .unwrap();
        std::fs::write(evidence.join("instruction-refusal.stderr"), &refused.stderr).unwrap();
        assert_eq!(refused.status.code(), Some(127), "{refused:?}");
        assert!(
            refused.stdout.is_empty() && refused.stderr.is_empty(),
            "{refused:?}"
        );
    }
    let symbols = Command::new("nm").arg(library).output().unwrap();
    assert!(symbols.status.success());
    let symbols = String::from_utf8(symbols.stdout).unwrap();
    let mut windows = vec![(0, String::new())];
    let mut targets = vec![
        (1, "reverie_liteinst_clock_disable_published"),
        (2, "reverie_liteinst_clock_enable_published"),
        (3, "reverie_liteinst_clock_handoff_published"),
    ];
    if std::env::var_os("CLOCK_FIXTURE_NATIVE_SCOPE").is_some() {
        targets.extend([
            (4, "reverie_liteinst_instruction_get_cpuid"),
            (5, "reverie_liteinst_instruction_get_tsc"),
            (6, "reverie_liteinst_instruction_set_cpuid"),
            (7, "reverie_liteinst_instruction_set_tsc"),
            (8, "reverie_liteinst_instruction_restore_tsc"),
            (9, "reverie_liteinst_instruction_restore_cpuid"),
            (10, "reverie_liteinst_instruction_get_cpuid_returned"),
            (11, "reverie_liteinst_instruction_get_tsc_returned"),
            (12, "reverie_liteinst_instruction_set_cpuid_returned"),
            (13, "reverie_liteinst_instruction_set_tsc_returned"),
            (14, "reverie_liteinst_instruction_restore_tsc_returned"),
            (15, "reverie_liteinst_instruction_restore_cpuid_returned"),
        ]);
    }
    for (kind, name) in targets {
        let address = symbols
            .lines()
            .find_map(|line| {
                let fields: Vec<_> = line.split_whitespace().collect();
                (fields.len() == 3 && fields[2] == name).then(|| fields[0].to_owned())
            })
            .expect("actual linked window symbol");
        windows.push((kind, address));
    }
    for repetition in 0..20 {
        for (kind, window) in &windows {
            for work in [0, 100, 10000] {
                let mut command = Command::new(guest);
                if std::env::var_os("CLOCK_FIXTURE_SUD").is_some() {
                    unsafe {
                        command.pre_exec(|| {
                            let mask =
                                (1u64 << (libc::SIGUSR1 - 1)) | (1u64 << (libc::SIGALRM - 1));
                            if libc::syscall(
                                libc::SYS_rt_sigprocmask,
                                libc::SIG_BLOCK,
                                &raw const mask,
                                0u64,
                                8u64,
                            ) != 0
                            {
                                return Err(std::io::Error::last_os_error());
                            }
                            Ok(())
                        });
                    }
                }
                command
                    .env("LD_PRELOAD", library)
                    .env("CLOCK_FIXTURE_SOCKET", &socket)
                    .env("CLOCK_FIXTURE_WORK", work.to_string())
                    .env_remove("CLOCK_FIXTURE_WINDOW");
                if *kind != 0 {
                    command
                        .env("CLOCK_FIXTURE_WINDOW", window)
                        .env("CLOCK_FIXTURE_WINDOW_KIND", kind.to_string());
                }
                let output = finish(
                    command
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .spawn()
                        .unwrap(),
                );
                let name = format!("run-{repetition}-window-{kind}-work-{work}");
                std::fs::write(evidence.join(format!("{name}.stdout")), &output.stdout).unwrap();
                std::fs::write(evidence.join(format!("{name}.stderr")), &output.stderr).unwrap();
                std::fs::write(
                    evidence.join(format!("{name}.status")),
                    output.status.to_string(),
                )
                .unwrap();
                assert!(output.status.success(), "{name}: {output:?}");
                assert_eq!(output.stdout.len(), 48, "six real guest samples: {name}");
                let trajectory: Vec<u64> = output
                    .stdout
                    .as_chunks::<8>()
                    .0
                    .iter()
                    .map(|bytes| u64::from_le_bytes(*bytes))
                    .collect();
                let deltas: Vec<_> = trajectory
                    .windows(2)
                    .map(|pair| pair[1].checked_sub(pair[0]).unwrap())
                    .collect();
                if std::env::var_os("CLOCK_FIXTURE_SUD").is_some() {
                    assert_eq!(
                        trajectory,
                        [7, 8, 9, 10, 17, 18],
                        "unchanged complete guest trajectory: {name}"
                    );
                }
                assert_eq!(
                    deltas,
                    [1, 1, 1, 7, 1],
                    "exact unmodified guest instructions: {trajectory:?}"
                );
                if let Some(reference) = &reference {
                    assert_eq!(
                        &trajectory, reference,
                        "full trajectory including first interval: {name}"
                    );
                } else {
                    reference = Some(trajectory.clone());
                }
                println!("{name} full-trajectory={trajectory:?}");
            }
        }
    }
    drop(coordinator);
    std::fs::remove_file(socket).unwrap();
}
