// SOURCE 39ac9f8a55f1b4d17b8c4c0001ce6c4d8a763435:detcore/src/lib.rs lines 1285..1335; SHA256 5dcad52c85ca1a6dc2e1ab93f484ea54142e3aebf58cf26fe25dd55814572554
    async fn handle_cpuid_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        eax: u32,
        ecx: u32,
    ) -> Result<CpuIdResult, Errno> {
        trace!("handle_cpuid_event: eax: {}, ecx: {}", eax, ecx);
        self.pre_handler_hook(guest, false).await;
        let res = if self.cfg.virtualize_cpuid {
            let dettid = guest.thread_state().dettid;
            let time = &mut guest.thread_state_mut().thread_logical_time;
            let intercepted = cpuid::InterceptedCpuid::new();
            time.add_cpuid();
            let nanos = time.as_nanos();
            trace!(
                "[dtid {}] inbound cpuid, new logical time: {:?}",
                dettid, time
            );
            if self.cfg.should_trace_schedevent() {
                trace_schedevent(
                    guest,
                    SchedEvent {
                        dettid,
                        op: Op::Cpuid,
                        count: 1,
                        start_rip: None,
                        end_rip: None,
                        end_time: Some(nanos),
                    },
                    true,
                )
                .await;
            }
            intercepted.cpuid(eax, ecx).unwrap_or_else(|| {
                warn!(
                    "[dtid {}] cpuid leaf 0x{:x} subleaf 0x{:x} not in deterministic table; returning zero result",
                    dettid, eax, ecx
                );
                CpuIdResult {
                    eax: 0,
                    ebx: 0,
                    ecx: 0,
                    edx: 0,
                }
            })
        } else {
            cpuid!(eax, ecx)
        };
        self.post_handler_hook(guest).await;
        Ok(res)
    }

