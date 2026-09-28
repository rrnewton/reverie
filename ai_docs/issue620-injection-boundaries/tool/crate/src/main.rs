use std::io::Write;
use std::sync::Mutex;
use reverie::{Error, ExitStatus, GlobalTool, Guest, Pid, Signal, Subscription, Tool};
use reverie::process::{Command, Stdio};
use reverie::syscalls::{Addr, AddrMut, Errno, Getpid, MemoryAccess, Ppoll, RtSigprocmask, Syscall, Sysno};
use reverie_ptrace::{ToolRunOutcome, TracerBuilder};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
enum Route { #[default] Exact, Scratch }
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
enum Next { #[default] Getpid, MaskQuery, SecondWait }
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
struct Config { route: Route, next: Next }
#[derive(Default)]
struct Log { rows: Mutex<Vec<Value>> }
fn emit(value: &Value) {
    println!("{}", value);
    std::io::stdout().flush().expect("retain diagnostic event");
}
#[reverie::global_tool]
impl GlobalTool for Log {
    type Request = String;
    type Response = ();
    type Config = Config;
    async fn receive_rpc(&self, from: Pid, encoded: String) {
        let value: Value = serde_json::from_str(&encoded).expect("diagnostic RPC must carry valid JSON");
        let row = json!({"event":"tool_rpc", "from":from.as_raw(), "record":value});
        emit(&row);
        self.rows.lock().unwrap().push(row);
    }
}
fn identity(pid: i32) -> std::io::Result<u64> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let fields: Vec<_> = text.rsplit_once(')').expect("proc stat delimiter").1.split_whitespace().collect();
    Ok(fields[19].parse().expect("proc start ticks"))
}
fn snapshot<G: Guest<Observe>>(guest: &G, base: usize) -> Result<Value, Error> {
    let tid = guest.tid();
    let status = std::fs::read_to_string(format!("/proc/{}/status", tid.as_raw()))
        .map_err(|e| Error::Io(std::io::Error::new(e.kind(), format!("read diagnostic status for TID {}: {e}", tid.as_raw()))))?;
    let fields: Vec<_> = status.lines().filter(|line| ["SigBlk:", "SigPnd:", "ShdPnd:"].iter().any(|p| line.starts_with(p))).collect();
    let query: u64 = guest.memory().read_value(Addr::from_raw(base + 64).ok_or(Errno::EFAULT)?)?;
    let start = identity(tid.as_raw()).map_err(|e| Error::Io(std::io::Error::new(e.kind(), format!("read diagnostic identity for TID {}: {e}", tid.as_raw()))))?;
    Ok(json!({"tid":tid.as_raw(),"start":start,"proc_status":fields,"query_word":query}))
}
fn public_result(result: Result<i64, Errno>) -> Value {
    match result { Ok(value) => json!({"ok":value}), Err(error) => json!({"errno":error.into_raw()}) }
}
#[derive(Clone, Copy, Debug, Default)]
struct Observe;
#[reverie::tool]
impl Tool for Observe {
    type GlobalState = Log;
    type ThreadState = u64;
    fn subscriptions(_: &Config) -> Subscription {
        let mut s = Subscription::none(); s.syscall(Sysno::ppoll); s
    }
    async fn handle_syscall_event<G: Guest<Self>>(&self, guest: &mut G, call: Syscall) -> Result<i64, Error> {
        let config = *guest.config();
        let Syscall::Ppoll(original) = call else { return Ok(guest.inject(call).await?); };
        let base = original.timeout().ok_or(Errno::EFAULT)?.as_raw();
        *guest.thread_state_mut() += 1;
        let callback = *guest.thread_state();
        guest.send_rpc(json!({"kind":"callback_entry","callback":callback,"config":config,"snapshot":snapshot(guest,base)?}).to_string()).await;
        // Exact forwards the actual pending (number, arguments); scratch changes
        // only the timeout pointer to distinct, equally initialized zero storage.
        let a = match config.route {
            Route::Exact => guest.inject(original).await,
            Route::Scratch => guest.inject(original.with_timeout(AddrMut::from_raw(base + 16))).await,
        };
        guest.send_rpc(json!({"kind":"A_result","callback":callback,"result":public_result(a),"snapshot":snapshot(guest,base)?}).to_string()).await;
        // One real public injection. Never inject_with_retry, never hide an
        // ERESTARTSYS before B executes, and no observation-only extra syscall.
        let b = match config.next {
            Next::Getpid => guest.inject(Getpid::new()).await,
            Next::MaskQuery => guest.inject(RtSigprocmask::new().with_how(libc::SIG_SETMASK).with_set(None).with_oldset(AddrMut::from_raw(base + 64)).with_sigsetsize(8)).await,
            Next::SecondWait => guest.inject(Ppoll::new().with_fds(None).with_nfds(0).with_timeout(AddrMut::from_raw(base + 32)).with_sigmask(Addr::from_raw(base + 56)).with_sigsetsize(8)).await,
        };
        guest.send_rpc(json!({"kind":"B_result","callback":callback,"result":public_result(b),"snapshot":snapshot(guest,base)?}).to_string()).await;
        // At most one B2 per cell, on the first callback only. Preserve any
        // later kernel-driven callback reentry; never retry B2 or loop to green.
        if callback == 1 && b == Err(Errno::ERESTARTSYS) {
            let b2 = match config.next {
                Next::Getpid => guest.inject(Getpid::new()).await,
                Next::MaskQuery => guest.inject(RtSigprocmask::new().with_how(libc::SIG_SETMASK).with_set(None).with_oldset(AddrMut::from_raw(base + 64)).with_sigsetsize(8)).await,
                Next::SecondWait => guest.inject(Ppoll::new().with_fds(None).with_nfds(0).with_timeout(AddrMut::from_raw(base + 32)).with_sigmask(Addr::from_raw(base + 56)).with_sigsetsize(8)).await,
            };
            guest.send_rpc(json!({"kind":"B2_result","callback":callback,"result":public_result(b2),"snapshot":snapshot(guest,base)?}).to_string()).await;
        }
        guest.send_rpc(json!({"kind":"callback_return_A_unchanged","callback":callback,"result":public_result(a)}).to_string()).await;
        Ok(a?)
    }
    async fn handle_signal_event<G: Guest<Self>>(&self, guest: &mut G, signal: Signal) -> Result<Option<Signal>, Errno> {
        guest.send_rpc(json!({"kind":"signal_hook","signal":signal as i32,"disposition":"forward unchanged"}).to_string()).await;
        Ok(Some(signal))
    }
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    assert_eq!(args.len(), 4, "usage: probe GUEST exact|scratch getpid|mask-query|second-wait");
    let config = Config {
        route: match args[2].as_str() { "exact" => Route::Exact, "scratch" => Route::Scratch, _ => panic!("unknown route") },
        next: match args[3].as_str() { "getpid" => Next::Getpid, "mask-query" => Next::MaskQuery, "second-wait" => Next::SecondWait, _ => panic!("unknown B") },
    };
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let code = runtime.block_on(tokio::task::LocalSet::new().run_until(async {
        let mut command = Command::new(&args[1]);
        command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let tracer = match TracerBuilder::<Observe>::new(command).config(config).spawn().await {
            Ok(t) => t,
            Err(e) => { emit(&json!({"event":"spawn_failure","error":format!("{e:?}")})); return 1; }
        };
        let pid = tracer.guest_pid().as_raw();
        let start = identity(pid).expect("live exact tracee identity before wait");
        emit(&json!({"event":"tracee_owned","pid":pid,"start":start,"config":config}));
        let initial = tracer.wait_with_output_completion().await;
        let mut had_pending_cleanup = false;
        let outcome = match initial {
            ToolRunOutcome::CleanupPending(pending) => {
                had_pending_cleanup = true;
                emit(&json!({"event":"cleanup_pending_initial","failure":format!("{:?}",pending.failure()),"callback_diagnostics":format!("{:?}",pending.callback_diagnostics()),"complete":false}));
                // Public same-owner cleanup only: does not restart a callback,
                // reconstruct task state, or rerun the failed reader.
                pending.resume_cleanup().await
            }
            outcome => outcome,
        };
        match outcome {
            ToolRunOutcome::Complete(completion) => {
                emit(&json!({"event":"callback_diagnostics","records":format!("{:?}",completion.callback_diagnostics())}));
                let after = match identity(pid) {
                    Ok(s) => Some(s),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                    Err(e) => { emit(&json!({"event":"retirement_read_failure","error":e.to_string()})); return 1; }
                };
                let gone = after != Some(start);
                let rows = completion.global_state.rows.lock().unwrap();
                let hooks = rows.iter().filter(|r| r["record"]["kind"] == "signal_hook").count();
                match completion.result {
                    Ok(output) => {
                        emit(&json!({"event":"complete","config":config,"pid":pid,"start":start,"original_generation_gone":gone,"current_start":after,"status":format!("{:?}",output.status),"guest_stdout":output.stdout,"guest_stderr":output.stderr,"signal_hook_count":hooks,"records":rows.len(),"product_qualified":false,"had_pending_cleanup":had_pending_cleanup}));
                        if gone && output.status == ExitStatus::Exited(0) && output.stderr.is_empty() && !had_pending_cleanup { 0 } else { 1 }
                    }
                    Err(failure) => { emit(&json!({"event":"backend_failure","failure":format!("{failure:?}"),"original_generation_gone":gone,"signal_hook_count":hooks,"captured_stdout":failure.captured_prefix().map(|p|p.stdout()),"captured_stderr":failure.captured_prefix().map(|p|p.stderr()),"had_pending_cleanup":had_pending_cleanup})); 1 }
                }
            }
            ToolRunOutcome::CleanupPending(pending) => {
                emit(&json!({"event":"cleanup_pending","failure":format!("{:?}",pending.failure()),"complete":false}));
                // Preserve the incomplete owner until process exit. The outer
                // bounded scope, not dropping this value, must prove retirement.
                std::mem::forget(pending); 1
            }
            ToolRunOutcome::UnsupportedBackend(tracer) => {
                emit(&json!({"event":"unsupported_completion","complete":false}));
                std::mem::forget(tracer); 1
            }
        }
    }));
    std::process::exit(code);
}
