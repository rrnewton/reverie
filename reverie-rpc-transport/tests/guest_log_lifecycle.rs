use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::time::Duration;

use reverie_rpc_transport::guest_log::Options;
use reverie_rpc_transport::guest_log::Phase;
use reverie_rpc_transport::guest_log::PublishError;
use reverie_rpc_transport::guest_log::SharedBuffer;
use reverie_rpc_transport::guest_log::channel_pair;
use reverie_rpc_transport::guest_log::retained_log;

fn wait(mapping: &SharedBuffer, _: u32) -> Result<(), PublishError> {
    if mapping.stopped() {
        return Err(PublishError::Stopped);
    }
    std::thread::sleep(Duration::from_millis(1));
    Ok(())
}

fn producer(mode: &str, fd: i32) {
    let mapping = SharedBuffer::receive(fd).unwrap();
    let mut parent = unsafe { mapping.activate(0, i64::from(libc::getpid())) }.unwrap();
    parent
        .write_record(&mapping, b"parent\0record", wait)
        .unwrap();
    let slot = mapping.reserve_child(0).unwrap();
    let result = if mode == "failed-fork" {
        unsafe { libc::syscall(libc::SYS_clone, libc::CLONE_THREAD, 0, 0, 0, 0) }
    } else {
        i64::from(unsafe { libc::fork() })
    };
    if result == 0 {
        if mode == "death-before-attach" {
            unsafe {
                libc::_exit(0);
            }
        }
        let mut child = unsafe { mapping.activate(slot, i64::from(libc::getpid())) }.unwrap();
        if mode == "parent-first-exit" {
            std::thread::sleep(Duration::from_millis(50));
        }
        child
            .write_record(&mapping, b"child\xffrecord", wait)
            .unwrap();
        child.finish(&mapping, wait).unwrap();
        unsafe {
            libc::_exit(0);
        }
    }
    if mode == "failed-fork" {
        assert_eq!(result, -1);
    } else {
        assert!(result > 0);
    }
    mapping.resolve_fork(slot, result).unwrap();
    if mode == "wait-error" {
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(i32::MAX, &mut status, libc::WNOHANG) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }
    parent.finish(&mapping, wait).unwrap();
    if mode != "parent-first-exit" && result > 0 {
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(result as i32, &mut status, 0) },
            result as i32
        );
        assert_eq!(status, 0);
    }
}

fn main() {
    let arguments: Vec<String> = std::env::args().collect();
    if arguments.get(1).map(String::as_str) == Some("--producer") {
        producer(&arguments[2], arguments[3].parse().unwrap());
        return;
    }
    for mode in [
        "fork",
        "wait-error",
        "failed-fork",
        "death-before-attach",
        "parent-first-exit",
    ] {
        let options = Options {
            byte_limit: 4096,
            producers: 4,
            slots: 1,
        };
        let (host, guest) = channel_pair(options).unwrap();
        let (sink, handle) = retained_log(options);
        let reader = sink.reader(host).unwrap();
        let descriptor = guest.as_raw_fd();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.args(["--producer", mode, &descriptor.to_string()]);
        unsafe {
            command.pre_exec(move || {
                if libc::fcntl(descriptor, libc::F_SETFD, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        drop(guest);
        let collect = std::thread::spawn(move || reader.run());
        assert!(child.wait().unwrap().success());
        handle.root_reaped();
        let report = collect.join().unwrap();
        assert!(
            report.peer_closed && report.root_reaped,
            "{mode}: {report:?}"
        );
        assert_eq!(report.streams[0].bytes, b"parent\0record", "{mode}");
        if mode == "death-before-attach" {
            assert_eq!(report.phase, Phase::Incomplete);
        } else {
            assert_eq!(report.phase, Phase::Complete, "{mode}: {report:?}");
            if mode != "failed-fork" {
                assert_eq!(report.streams[1].bytes, b"child\xffrecord");
            }
        }
        println!(
            "lifecycle {mode}: expected phase={:?}, streams={}, root_reaped={}, peer_closed={}",
            report.phase,
            report.streams.len(),
            report.root_reaped,
            report.peer_closed
        );
    }
    println!("5 ordinary subprocess lifecycle cases passed; no instrumented guest executed");
}
