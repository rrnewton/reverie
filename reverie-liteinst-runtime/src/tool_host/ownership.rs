use std::future::poll_fn;
use std::io;
use std::sync::Arc;
use std::task::Poll;
use std::task::Waker;

use reverie::Pid;
use reverie::Tool;

use super::CoordinatorRpc;
use super::ScratchOwner;
use super::SpinMutex;
use crate::rpc::BoundRpc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Identity {
    pid: Pid,
    tid: Pid,
    incarnation: u64,
    generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Initializing,
    Available,
    Borrowed,
    Retiring,
    Retired,
}

struct Entry<T: Tool, Client: BoundRpc<T::GlobalState>> {
    identity: Identity,
    phase: Phase,
    state: Option<T::ThreadState>,
    rpc: Option<Arc<Client>>,
    next: Option<Box<Entry<T, Client>>>,
}

struct Process<T: Tool, Client: BoundRpc<T::GlobalState>> {
    pid: Pid,
    incarnation: u64,
    generation: u64,
    closing: bool,
    finalized: bool,
    tool: Option<Arc<T>>,
    entries: Option<Box<Entry<T, Client>>>,
    leases: usize,
    waiter: Option<Waker>,
}

impl<T: Tool, Client: BoundRpc<T::GlobalState>> Process<T, Client> {
    fn entry(&mut self, tid: Pid) -> Option<&mut Entry<T, Client>> {
        let mut next = self.entries.as_deref_mut();
        while let Some(entry) = next {
            if entry.identity.tid == tid {
                return Some(entry);
            }
            next = entry.next.as_deref_mut();
        }
        None
    }

    fn check(&self, pid: Pid) -> io::Result<()> {
        if self.pid != pid || self.closing || self.finalized || self.tool.is_none() {
            return Err(io::Error::other("callback process is unavailable"));
        }
        Ok(())
    }

    fn check_parent(&mut self, parent: Identity) -> io::Result<()> {
        self.check(parent.pid)?;
        if self.incarnation != parent.incarnation {
            return Err(io::Error::other("parent callback incarnation mismatch"));
        }
        let entry = self
            .entry(parent.tid)
            .ok_or_else(|| io::Error::other("parent callback missing"))?;
        if entry.identity != parent || entry.phase != Phase::Borrowed {
            return Err(io::Error::other("parent callback no longer borrowed"));
        }
        Ok(())
    }
}

pub(super) struct Registry<
    T: Tool,
    Client: BoundRpc<T::GlobalState> = CoordinatorRpc<<T as Tool>::GlobalState>,
> {
    process: SpinMutex<Process<T, Client>>,
    resources: crate::rpc::resources::Resources,
}

pub(super) struct Invocation<
    'host,
    T: Tool,
    Client: BoundRpc<T::GlobalState> = CoordinatorRpc<<T as Tool>::GlobalState>,
> {
    registry: &'host Registry<T, Client>,
    identity: Identity,
    tool: Option<Arc<T>>,
    state: Option<T::ThreadState>,
    rpc: Option<Arc<Client>>,
    scratch: ScratchOwner,
    parent: Option<Identity>,
    retire_on_release: bool,
}

struct Cleanup<'host, T: Tool, Client: BoundRpc<T::GlobalState>> {
    registry: &'host Registry<T, Client>,
    identity: Identity,
    retire: bool,
}

impl<T: Tool, Client: BoundRpc<T::GlobalState>> Drop for Cleanup<'_, T, Client> {
    fn drop(&mut self) {
        let waiter = {
            let mut process = self.registry.process.lock();
            if self.retire {
                let entry = process.entry(self.identity.tid).unwrap();
                assert_eq!(entry.identity, self.identity);
                assert_eq!(entry.phase, Phase::Retiring);
                entry.phase = Phase::Retired;
            }
            process.leases = process
                .leases
                .checked_sub(1)
                .expect("callback lease underflow");
            if process.leases == 0 {
                process.waiter.take()
            } else {
                None
            }
        };
        if let Some(waiter) = waiter {
            waiter.wake();
        }
    }
}

impl<T: Tool, Client: BoundRpc<T::GlobalState>> Registry<T, Client> {
    #[cfg(test)]
    pub(super) fn new(pid: Pid, tool: T) -> Self {
        Self::with_resources(pid, tool, crate::rpc::resources::Resources::new())
    }

    pub(super) fn with_resources(
        pid: Pid,
        tool: T,
        resources: crate::rpc::resources::Resources,
    ) -> Self {
        Self {
            resources,
            process: SpinMutex::new(Process {
                pid,
                incarnation: 1,
                generation: 0,
                closing: false,
                finalized: false,
                tool: Some(Arc::new(tool)),
                entries: None,
                leases: 0,
                waiter: None,
            }),
        }
    }

    pub(super) fn terminal_resource(&self) -> io::Result<crate::rpc::resources::Lease> {
        self.resources.acquire()
    }

    pub(super) async fn drain_resources(&self) -> io::Result<()> {
        self.resources.drain().await
    }

    pub(super) fn unstarted(&self, pid: Pid) -> bool {
        let process = self.process.lock();
        process.check(pid).is_ok() && process.entries.is_none() && process.leases == 0
    }

    pub(super) fn replace_fork_process(&self, parent: Pid, child: Pid, tool: T) -> io::Result<()> {
        let tool = Arc::new(tool);
        let old = {
            let mut process = self.process.lock();
            if process.pid != parent || process.closing || process.finalized || process.leases != 0
            {
                return Err(io::Error::other(
                    "fork inherited outstanding callback ownership",
                ));
            }
            let mut next = process.entries.as_deref();
            while let Some(entry) = next {
                if entry.phase != Phase::Retired {
                    return Err(io::Error::other("fork inherited a live entry"));
                }
                next = entry.next.as_deref();
            }
            let incarnation = process
                .incarnation
                .checked_add(1)
                .ok_or_else(|| io::Error::other("fork incarnation exhausted"))?;
            let generation = process.generation;
            core::mem::replace(
                &mut *process,
                Process {
                    pid: child,
                    incarnation,
                    generation,
                    closing: false,
                    finalized: false,
                    tool: Some(tool),
                    entries: None,
                    leases: 0,
                    waiter: None,
                },
            )
        };
        drop(old);
        Ok(())
    }

    pub(super) fn reserve(
        &self,
        pid: Pid,
        tid: Pid,
        rpc: Arc<Client>,
        scratch: &ScratchOwner,
        root: bool,
    ) -> io::Result<Invocation<'_, T, Client>> {
        self.reserve_with_parent(pid, tid, rpc, scratch, root, None)
    }

    fn reserve_with_parent(
        &self,
        pid: Pid,
        tid: Pid,
        rpc: Arc<Client>,
        scratch: &ScratchOwner,
        root: bool,
        parent: Option<Identity>,
    ) -> io::Result<Invocation<'_, T, Client>> {
        if root == parent.is_some() {
            return Err(io::Error::other(
                "callback reservation requires root or borrowed parent",
            ));
        }
        if rpc.identity() != (pid, tid) {
            return Err(io::Error::other("callback client identity mismatch"));
        }
        let mut entry = Box::new(Entry {
            identity: Identity {
                pid,
                tid,
                incarnation: 0,
                generation: 0,
            },
            phase: Phase::Initializing,
            state: None,
            rpc: Some(rpc.clone()),
            next: None,
        });
        let mut process = self.process.lock();
        process.check(pid)?;
        if let Some(parent) = parent {
            process.check_parent(parent)?;
        }
        if process.entry(tid).is_some() || (root && process.entries.is_some()) {
            return Err(io::Error::other("callback entry already reserved"));
        }
        let generation = process
            .generation
            .checked_add(1)
            .ok_or_else(|| io::Error::other("callback generation exhausted"))?;
        let leases = process
            .leases
            .checked_add(1)
            .ok_or_else(|| io::Error::other("callback lease count exhausted"))?;
        let identity = Identity {
            pid,
            tid,
            incarnation: process.incarnation,
            generation,
        };
        entry.identity = identity;
        entry.next = process.entries.take();
        process.entries = Some(entry);
        process.generation = generation;
        process.leases = leases;
        let tool = process.tool.as_ref().unwrap().clone();
        drop(process);
        Ok(Invocation {
            registry: self,
            identity,
            tool: Some(tool),
            state: None,
            rpc: Some(rpc),
            scratch: scratch.clone(),
            parent,
            retire_on_release: true,
        })
    }

    pub(super) fn identity(&self, pid: Pid, tid: Pid) -> io::Result<Identity> {
        let mut process = self.process.lock();
        process.check(pid)?;
        process
            .entry(tid)
            .map(|entry| entry.identity)
            .ok_or_else(|| io::Error::other("callback entry is missing"))
    }

    pub(super) fn acquire(
        &self,
        identity: Identity,
        scratch: &ScratchOwner,
    ) -> io::Result<Invocation<'_, T, Client>> {
        let mut process = self.process.lock();
        process.check(identity.pid)?;
        if process.incarnation != identity.incarnation {
            return Err(io::Error::other("callback incarnation mismatch"));
        }
        let leases = process
            .leases
            .checked_add(1)
            .ok_or_else(|| io::Error::other("callback lease count exhausted"))?;
        let entry = process
            .entry(identity.tid)
            .ok_or_else(|| io::Error::other("callback entry is missing"))?;
        if entry.identity != identity || entry.phase != Phase::Available {
            return Err(io::Error::other("callback entry stale, busy, or retired"));
        }
        let rpc = entry
            .rpc
            .as_ref()
            .ok_or_else(|| io::Error::other("callback client retired"))?
            .clone();
        let state = entry
            .state
            .take()
            .ok_or_else(|| io::Error::other("callback state missing"))?;
        entry.phase = Phase::Borrowed;
        process.leases = leases;
        let tool = process.tool.as_ref().unwrap().clone();
        drop(process);
        Ok(Invocation {
            registry: self,
            identity,
            tool: Some(tool),
            state: Some(state),
            rpc: Some(rpc),
            scratch: scratch.clone(),
            parent: None,
            retire_on_release: true,
        })
    }

    pub(super) fn retire(&self, identity: Identity) -> io::Result<()> {
        let (cleanup, payload) = {
            let mut process = self.process.lock();
            if process.pid != identity.pid || process.incarnation != identity.incarnation {
                return Err(io::Error::other("callback retirement identity mismatch"));
            }
            let leases = process
                .leases
                .checked_add(1)
                .ok_or_else(|| io::Error::other("callback lease count exhausted"))?;
            let entry = process
                .entry(identity.tid)
                .ok_or_else(|| io::Error::other("callback entry missing"))?;
            if entry.identity != identity || matches!(entry.phase, Phase::Retiring | Phase::Retired)
            {
                return Err(io::Error::other("callback entry already retired"));
            }
            let available = entry.phase == Phase::Available;
            entry.phase = Phase::Retiring;
            if !available {
                return Ok(());
            }
            let payload = (entry.state.take(), entry.rpc.take());
            process.leases = leases;
            (
                Cleanup {
                    registry: self,
                    identity,
                    retire: true,
                },
                payload,
            )
        };
        drop(payload);
        drop(cleanup);
        Ok(())
    }

    pub(super) fn retained_entries(&self) -> usize {
        let process = self.process.lock();
        let mut next = process.entries.as_deref();
        let mut count = 0;
        while let Some(entry) = next {
            if entry.phase != Phase::Retired {
                count += 1;
            }
            next = entry.next.as_deref();
        }
        count
    }

    #[cfg(test)]
    pub(super) fn active_entries(&self) -> usize {
        let process = self.process.lock();
        let mut next = process.entries.as_deref();
        let mut count = 0;
        while let Some(entry) = next {
            if !matches!(entry.phase, Phase::Retired | Phase::Retiring) {
                count += 1;
            }
            next = entry.next.as_deref();
        }
        count
    }

    #[cfg(test)]
    pub(super) fn tool_present(&self) -> bool {
        self.process.lock().tool.is_some()
    }

    #[cfg(test)]
    pub(super) fn entry_count(&self) -> usize {
        let process = self.process.lock();
        let mut next = process.entries.as_deref();
        let mut count = 0;
        while let Some(entry) = next {
            count += 1;
            next = entry.next.as_deref();
        }
        count
    }

    pub(super) fn close(&self, identity: Identity) -> io::Result<()> {
        let mut process = self.process.lock();
        process.check(identity.pid)?;
        let entry = process
            .entry(identity.tid)
            .ok_or_else(|| io::Error::other("closing callback missing"))?;
        if entry.identity != identity || entry.phase != Phase::Borrowed {
            return Err(io::Error::other("closing callback ownership mismatch"));
        }
        process.closing = true;
        drop(process);
        self.resources.close();
        self.retire(identity)
    }

    pub(super) async fn consume(&self, pid: Pid) -> io::Result<T> {
        poll_fn(|context| {
            let replacement = context.waker().clone();
            let mut process = self.process.lock();
            if process.pid != pid || !process.closing || process.finalized {
                return Poll::Ready(Err(io::Error::other("process finalization unavailable")));
            }
            if process.leases == 0 {
                return Poll::Ready(Ok(()));
            }
            let old = process.waiter.replace(replacement);
            drop(process);
            drop(old);
            Poll::Pending
        })
        .await?;
        let (tool, entries) = {
            let mut process = self.process.lock();
            if process.pid != pid || !process.closing || process.finalized || process.leases != 0 {
                return Err(io::Error::other("process finalization changed"));
            }
            process.finalized = true;
            (process.tool.take().unwrap(), process.entries.take())
        };
        drop(entries);
        Arc::try_unwrap(tool).map_err(|_| io::Error::other("uncounted Tool ownership at exit"))
    }
}

impl<T: Tool, Client: BoundRpc<T::GlobalState>> Invocation<'_, T, Client> {
    pub(super) fn identity(&self) -> Identity {
        self.identity
    }

    pub(super) fn initialize(&mut self, parent: Option<(Pid, &T::ThreadState)>) -> io::Result<()> {
        if self.state.is_some() {
            return Err(io::Error::other("callback initialized twice"));
        }
        let state = self
            .tool
            .as_ref()
            .unwrap()
            .init_thread_state(self.identity.tid, parent);
        self.install_state(state)
    }

    pub(super) fn install_state(&mut self, state: T::ThreadState) -> io::Result<()> {
        if self.state.is_some() {
            return Err(io::Error::other("callback initialized twice"));
        }
        self.state = Some(state);
        let mut process = self.registry.process.lock();
        process.check(self.identity.pid)?;
        let entry = process.entry(self.identity.tid).unwrap();
        if entry.identity != self.identity || entry.phase != Phase::Initializing {
            return Err(io::Error::other("callback initialization retired"));
        }
        entry.phase = Phase::Borrowed;
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn construct_child(
        &self,
        tid: Pid,
        rpc: Arc<Client>,
        scratch: &ScratchOwner,
    ) -> io::Result<Identity> {
        let mut child = self.registry.reserve_with_parent(
            self.identity.pid,
            tid,
            rpc,
            scratch,
            false,
            Some(self.identity),
        )?;
        child.initialize(Some((self.identity.tid, self.state.as_ref().unwrap())))?;
        let identity = child.identity;
        child.complete()?;
        Ok(identity)
    }

    pub(super) fn parts(&mut self) -> (&T, &mut T::ThreadState, &Client) {
        (
            self.tool.as_deref().unwrap(),
            self.state.as_mut().unwrap(),
            self.rpc.as_deref().unwrap(),
        )
    }

    pub(super) fn take_state(&mut self) -> T::ThreadState {
        self.state.take().unwrap()
    }

    pub(super) fn exit_parts(&self) -> (&T, &Client) {
        (self.tool.as_deref().unwrap(), self.rpc.as_deref().unwrap())
    }

    pub(super) fn client(&self) -> Arc<Client> {
        self.rpc.as_ref().unwrap().clone()
    }

    pub(super) fn complete(mut self) -> io::Result<()> {
        self.scratch.close();
        let mut process = self.registry.process.lock();
        process.check(self.identity.pid)?;
        if let Some(parent) = self.parent {
            process.check_parent(parent)?;
        }
        let entry = process.entry(self.identity.tid).unwrap();
        if entry.identity != self.identity
            || entry.phase != Phase::Borrowed
            || self.state.is_none()
            || entry.state.is_some()
        {
            return Err(io::Error::other("callback completion retired or invalid"));
        }
        entry.state = self.state.take();
        entry.phase = Phase::Available;
        self.retire_on_release = false;
        drop(process);
        #[cfg(test)]
        tests::after_publish();
        self.release();
        Ok(())
    }

    fn release(&mut self) {
        if self.tool.is_none() {
            return;
        }
        let rpc = {
            let mut process = self.registry.process.lock();
            if self.retire_on_release {
                let entry = process.entry(self.identity.tid).unwrap();
                assert_eq!(entry.identity, self.identity);
                assert!(matches!(
                    entry.phase,
                    Phase::Borrowed | Phase::Initializing | Phase::Retiring
                ));
                entry.phase = Phase::Retiring;
                entry.rpc.take()
            } else {
                None
            }
        };
        let cleanup = Cleanup {
            registry: self.registry,
            identity: self.identity,
            retire: self.retire_on_release,
        };
        let payload = (self.state.take(), self.rpc.take(), self.tool.take(), rpc);
        self.scratch.close();
        drop(payload);
        drop(cleanup);
    }
}

impl<T: Tool, Client: BoundRpc<T::GlobalState>> Drop for Invocation<'_, T, Client> {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests;
