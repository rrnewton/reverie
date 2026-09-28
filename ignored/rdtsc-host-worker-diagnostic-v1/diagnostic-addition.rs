// Diagnostic only: retain every timestamp callback and its actual saved user
// instruction pointer. The original fixture's exact two-call assertion remains.
#[derive(Debug, Default)]
struct HostTimestampDiagnosticLog {
    records: Mutex<Vec<(Pid, Rdtsc, u64)>>,
}

impl HostTimestampDiagnosticLog {
    fn calls(&self) -> Vec<(Pid, Rdtsc)> {
        self.records
            .lock()
            .expect("timestamp diagnostic lock poisoned")
            .iter()
            .map(|(pid, request, _)| (*pid, *request))
            .collect()
    }

    fn retain(&self) {
        let records = self.records.lock().expect("timestamp diagnostic lock poisoned");
        let bytes = serde_json::to_vec_pretty(&*records).unwrap();
        let directory = PathBuf::from(std::env::var_os("REVERIE_TIMESTAMP_DIAGNOSTIC_DIR").unwrap());
        std::fs::write(directory.join("callbacks.json"), &bytes).unwrap();
        eprintln!("timestamp diagnostic callbacks: {}", String::from_utf8(bytes).unwrap());
    }
}

#[reverie::global_tool]
impl GlobalTool for HostTimestampDiagnosticLog {
    type Request = (Rdtsc, u64);
    type Response = ();
    type Config = bool;

    async fn receive_rpc(&self, from: Pid, request: Self::Request) {
        self.records
            .lock()
            .expect("timestamp diagnostic lock poisoned")
            .push((from, request.0, request.1));
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct HostTimestampDiagnosticTool;

#[reverie::tool]
impl Tool for HostTimestampDiagnosticTool {
    type GlobalState = HostTimestampDiagnosticLog;
    type ThreadState = ();

    fn subscriptions(enabled: &bool) -> Subscription {
        TimestampTool::subscriptions(enabled)
    }

    async fn handle_rdtsc_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        request: Rdtsc,
    ) -> Result<RdtscResult, Errno> {
        let rip = guest.regs().await.rip;
        guest.send_rpc((request, rip)).await;
        Ok(match request {
            Rdtsc::Tsc => RdtscResult {
                tsc: RDTSC_SENTINEL,
                aux: None,
            },
            Rdtsc::Tscp => RdtscResult {
                tsc: RDTSCP_SENTINEL,
                aux: Some(RDTSCP_AUX_SENTINEL),
            },
        })
    }
}

fn compile_host_timestamp_diagnostic(directory: &std::path::Path, source: &str) -> PathBuf {
    let retained = PathBuf::from(std::env::var_os("REVERIE_TIMESTAMP_DIAGNOSTIC_DIR").unwrap());
    assert!(retained.is_dir());
    assert!(std::fs::read_dir(&retained).unwrap().next().is_none());
    let source_path = directory.join("timestamp-host-worker.c");
    let executable_path = directory.join("timestamp-host-worker");
    std::fs::write(&source_path, source).unwrap();
    std::fs::write(retained.join("timestamp-host-worker.c"), source).unwrap();
    let mut command = std::process::Command::new("/usr/bin/gcc");
    command
        .args(["-O2", "-pthread"])
        .arg(&source_path)
        .arg("-o")
        .arg(&executable_path);
    let argv: Vec<_> = std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|word| word.to_str().unwrap().to_owned())
        .collect();
    std::fs::write(
        retained.join("compiler-command.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "argv": argv,
            "cwd": std::env::current_dir().unwrap(),
        }))
        .unwrap(),
    )
    .unwrap();
    let output = command.output().unwrap();
    std::fs::write(retained.join("compiler.stdout"), &output.stdout).unwrap();
    std::fs::write(retained.join("compiler.stderr"), &output.stderr).unwrap();
    std::fs::write(
        retained.join("compiler-status.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "code": output.status.code(),
            "success": output.status.success(),
            "display": output.status.to_string(),
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "gcc failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let image = std::fs::read(&executable_path).unwrap();
    std::fs::write(retained.join("timestamp-host-worker.elf"), &image).unwrap();
    let elf = goblin::elf::Elf::parse(&image).unwrap();
    let interpreter = elf.interpreter.unwrap();
    let interpreter_path = std::fs::canonicalize(interpreter).unwrap();
    std::fs::write(
        retained.join("interpreter.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "pt_interp": interpreter,
            "resolved": interpreter_path,
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        retained.join("interpreter.elf"),
        std::fs::read(&interpreter_path).unwrap(),
    )
    .unwrap();
    executable_path
}

