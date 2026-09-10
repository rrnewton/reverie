use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use reverie::GlobalRPC;

use super::*;
use crate::tool_host::DispatchScratchScope;

type DropHook = Arc<dyn Fn() + Send + Sync>;

struct Client(Pid, Pid, Option<DropHook>);

impl Drop for Client {
    fn drop(&mut self) {
        if let Some(hook) = &self.2 {
            hook();
        }
    }
}

thread_local! {
    static AFTER_PUBLISH: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

pub(super) fn after_publish() {
    let hook = AFTER_PUBLISH.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

#[reverie::tool]
impl GlobalRPC<()> for Client {
    async fn send_rpc(&self, _: ()) {}
    fn config(&self) -> &() {
        &()
    }
}

impl BoundRpc<()> for Client {
    fn identity(&self) -> (Pid, Pid) {
        (self.0, self.1)
    }
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct State {
    value: usize,
    #[serde(skip)]
    dropped: Arc<AtomicUsize>,
    #[serde(skip)]
    registry: std::sync::Weak<Registry<Probe, Client>>,
    #[serde(skip)]
    on_drop: Option<DropHook>,
}

impl Drop for State {
    fn drop(&mut self) {
        if let Some(hook) = &self.on_drop {
            hook();
        }
        if let Some(registry) = self.registry.upgrade() {
            let _process = registry.process.lock();
            self.dropped.fetch_add(1, Ordering::SeqCst);
        } else {
            self.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }
}

#[derive(Default)]
struct Probe {
    constructions: Arc<AtomicUsize>,
    panic: AtomicBool,
}

#[reverie::tool]
impl Tool for Probe {
    type GlobalState = ();
    type ThreadState = State;

    fn init_thread_state(&self, _: Pid, parent: Option<(Pid, &State)>) -> State {
        self.constructions.fetch_add(1, Ordering::SeqCst);
        assert!(
            !self.panic.load(Ordering::SeqCst),
            "constructor panic control"
        );
        State {
            value: parent.map_or(3, |(_, parent)| parent.value),
            dropped: Arc::new(AtomicUsize::new(0)),
            registry: std::sync::Weak::new(),
            on_drop: None,
        }
    }
}

fn client(tid: i32) -> Arc<Client> {
    Arc::new(Client(Pid::from_raw(1), Pid::from_raw(tid), None))
}

fn registry() -> Arc<Registry<Probe, Client>> {
    Arc::new(Registry::new(Pid::from_raw(1), Probe::default()))
}

#[test]
fn exclusive_identity_claim_and_retirement_never_republish() {
    let registry = registry();
    let scope = DispatchScratchScope::enter();
    let mut invocation = registry
        .reserve(
            Pid::from_raw(1),
            Pid::from_raw(1),
            client(1),
            &scope.owner,
            true,
        )
        .unwrap();
    invocation.initialize(None).unwrap();
    let identity = invocation.identity();
    let dropped = invocation.state.as_mut().unwrap().dropped.clone();
    invocation.state.as_mut().unwrap().registry = Arc::downgrade(&registry);
    assert!(registry.acquire(identity, &scope.owner).is_err());
    assert_eq!(registry.process.lock().leases, 1);
    invocation.complete().unwrap();
    let scope = DispatchScratchScope::enter();
    assert!(
        registry
            .acquire(
                Identity {
                    generation: identity.generation + 1,
                    ..identity
                },
                &scope.owner
            )
            .is_err()
    );
    assert!(
        registry
            .acquire(
                Identity {
                    incarnation: identity.incarnation + 1,
                    ..identity
                },
                &scope.owner
            )
            .is_err()
    );
    assert!(
        registry
            .identity(Pid::from_raw(2), Pid::from_raw(1))
            .is_err()
    );
    assert!(
        registry
            .identity(Pid::from_raw(1), Pid::from_raw(2))
            .is_err()
    );
    let mut invocation = registry.acquire(identity, &scope.owner).unwrap();
    invocation.parts().1.value = 17;
    registry.retire(identity).unwrap();
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    assert!(invocation.complete().is_err());
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert!(registry.acquire(identity, &scope.owner).is_err());
    assert_eq!(registry.process.lock().leases, 0);
    assert_eq!(
        registry.process.lock().entry(identity.tid).unwrap().phase,
        Phase::Retired
    );
    assert!(
        registry
            .reserve(identity.pid, identity.tid, client(1), &scope.owner, true)
            .is_err()
    );
}

#[test]
fn constructor_unwind_is_terminal_and_not_a_lazy_root_retry() {
    let constructions = Arc::new(AtomicUsize::new(0));
    let registry = Registry::new(
        Pid::from_raw(1),
        Probe {
            constructions: constructions.clone(),
            panic: AtomicBool::new(true),
        },
    );
    let scope = DispatchScratchScope::enter();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut invocation = registry
            .reserve(
                Pid::from_raw(1),
                Pid::from_raw(1),
                client(1),
                &scope.owner,
                true,
            )
            .unwrap();
        invocation.initialize(None).unwrap();
    }));
    assert!(result.is_err());
    assert_eq!(constructions.load(Ordering::SeqCst), 1);
    assert_eq!(registry.process.lock().leases, 0);
    assert!(!registry.unstarted(Pid::from_raw(1)));
    assert!(
        registry
            .reserve(
                Pid::from_raw(1),
                Pid::from_raw(1),
                client(1),
                &scope.owner,
                true
            )
            .is_err()
    );
}

#[test]
fn canceled_initialization_rejects_publication_and_keeps_exact_client_identity() {
    let registry = registry();
    let scope = DispatchScratchScope::enter();
    assert!(
        registry
            .reserve(
                Pid::from_raw(1),
                Pid::from_raw(1),
                client(2),
                &scope.owner,
                true
            )
            .is_err()
    );
    assert!(registry.unstarted(Pid::from_raw(1)));
    let mut invocation = registry
        .reserve(
            Pid::from_raw(1),
            Pid::from_raw(1),
            client(1),
            &scope.owner,
            true,
        )
        .unwrap();
    let identity = invocation.identity();
    registry.retire(identity).unwrap();
    assert!(invocation.initialize(None).is_err());
    drop(invocation);
    assert_eq!(registry.process.lock().leases, 0);
    assert!(registry.acquire(identity, &scope.owner).is_err());
}

struct Notify(AtomicUsize);
impl std::task::Wake for Notify {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn consuming_tool_waits_for_explicit_lease_drain_and_cannot_replay() {
    let registry = registry();
    let root_scope = DispatchScratchScope::enter();
    let mut root = registry
        .reserve(
            Pid::from_raw(1),
            Pid::from_raw(1),
            client(1),
            &root_scope.owner,
            true,
        )
        .unwrap();
    root.initialize(None).unwrap();
    let child_scope = DispatchScratchScope::enter();
    let child_identity = root
        .construct_child(Pid::from_raw(2), client(2), &child_scope.owner)
        .unwrap();
    let current_scope = DispatchScratchScope::enter();
    let child = registry
        .acquire(child_identity, &current_scope.owner)
        .unwrap();
    registry.close(root.identity()).unwrap();
    drop(root);
    let mut finish = Box::pin(registry.consume(Pid::from_raw(1)));
    let notified = Arc::new(Notify(AtomicUsize::new(0)));
    let waker = std::task::Waker::from(notified.clone());
    let mut context = std::task::Context::from_waker(&waker);
    assert!(std::future::Future::poll(finish.as_mut(), &mut context).is_pending());
    assert!(
        registry
            .acquire(child_identity, &current_scope.owner)
            .is_err()
    );
    assert!(registry.tool_present());
    drop(child);
    assert_eq!(notified.0.load(Ordering::SeqCst), 1);
    let Poll::Ready(Ok(tool)) = std::future::Future::poll(finish.as_mut(), &mut context) else {
        panic!("final Tool owner not consumed");
    };
    assert_eq!(tool.constructions.load(Ordering::SeqCst), 2);
    assert!(!registry.tool_present());
    drop(finish);
    assert!(super::super::drive_ready(registry.consume(Pid::from_raw(1))).is_err());
}

#[test]
fn uncounted_tool_handle_is_terminal_not_refcount_polling() {
    let registry = registry();
    let scope = DispatchScratchScope::enter();
    let mut root = registry
        .reserve(
            Pid::from_raw(1),
            Pid::from_raw(1),
            client(1),
            &scope.owner,
            true,
        )
        .unwrap();
    root.initialize(None).unwrap();
    let unexpected = root.tool.as_ref().unwrap().clone();
    registry.close(root.identity()).unwrap();
    drop(root);
    assert!(super::super::drive_ready(registry.consume(Pid::from_raw(1))).is_err());
    assert!(!registry.tool_present());
    assert!(registry.process.lock().finalized);
    drop(unexpected);
}

#[test]
fn generation_exhaustion_and_live_fork_ownership_refuse_without_reset() {
    let registry = registry();
    let scope = DispatchScratchScope::enter();
    registry.process.lock().generation = u64::MAX;
    assert!(
        registry
            .reserve(
                Pid::from_raw(1),
                Pid::from_raw(1),
                client(1),
                &scope.owner,
                true
            )
            .is_err()
    );
    assert!(registry.unstarted(Pid::from_raw(1)));
    registry.process.lock().generation = 0;
    let mut root = registry
        .reserve(
            Pid::from_raw(1),
            Pid::from_raw(1),
            client(1),
            &scope.owner,
            true,
        )
        .unwrap();
    root.initialize(None).unwrap();
    assert!(
        registry
            .replace_fork_process(Pid::from_raw(1), Pid::from_raw(2), Probe::default())
            .is_err()
    );
    assert_eq!(registry.process.lock().pid, Pid::from_raw(1));
    drop(root);
    registry
        .replace_fork_process(Pid::from_raw(1), Pid::from_raw(2), Probe::default())
        .unwrap();
    assert!(registry.unstarted(Pid::from_raw(2)));
    assert_eq!(registry.process.lock().incarnation, 2);
}

#[test]
fn retired_parent_cannot_reserve_or_publish_a_child() {
    let registry = registry();
    let root_scope = DispatchScratchScope::enter();
    let mut root = registry
        .reserve(
            Pid::from_raw(1),
            Pid::from_raw(1),
            client(1),
            &root_scope.owner,
            true,
        )
        .unwrap();
    root.initialize(None).unwrap();
    let child_scope = DispatchScratchScope::enter();
    let mut child = registry
        .reserve_with_parent(
            Pid::from_raw(1),
            Pid::from_raw(2),
            client(2),
            &child_scope.owner,
            false,
            Some(root.identity()),
        )
        .unwrap();
    child
        .initialize(Some((Pid::from_raw(1), root.parts().1)))
        .unwrap();
    assert_eq!(registry.retained_entries(), 2);
    registry.retire(root.identity()).unwrap();
    assert_eq!(
        registry.retained_entries(),
        2,
        "retiring borrowed entries still have a lifetime"
    );
    assert!(
        root.construct_child(Pid::from_raw(3), client(3), &child_scope.owner)
            .is_err()
    );
    assert!(child.complete().is_err());
    assert_eq!(registry.retained_entries(), 1);
    assert_eq!(
        registry.entry_count(),
        2,
        "failed reservation must not publish an entry"
    );
    drop(root);
    assert_eq!(registry.retained_entries(), 0);
}

#[test]
fn published_completion_cannot_retire_a_new_borrower() {
    let registry = registry();
    let scope = DispatchScratchScope::enter();
    let mut root = registry
        .reserve(
            Pid::from_raw(1),
            Pid::from_raw(1),
            client(1),
            &scope.owner,
            true,
        )
        .unwrap();
    root.initialize(None).unwrap();
    let identity = root.identity();
    root.complete().unwrap();
    let published = Arc::new(std::sync::Barrier::new(2));
    let resume = Arc::new(std::sync::Barrier::new(2));
    std::thread::scope(|threads| {
        let registry_ref = &registry;
        let published_child = published.clone();
        let resume_child = resume.clone();
        let old = threads.spawn(move || {
            let scope = DispatchScratchScope::enter();
            let mut invocation = registry_ref.acquire(identity, &scope.owner).unwrap();
            invocation.parts().1.value = 17;
            AFTER_PUBLISH.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move || {
                    published_child.wait();
                    resume_child.wait();
                }))
            });
            invocation.complete().unwrap();
        });
        published.wait();
        let next_scope = DispatchScratchScope::enter();
        let mut next = registry.acquire(identity, &next_scope.owner).unwrap();
        assert_eq!(next.parts().1.value, 17);
        assert_eq!(registry.process.lock().leases, 2);
        resume.wait();
        old.join().unwrap();
        assert_eq!(registry.process.lock().leases, 1);
        assert_eq!(
            registry.process.lock().entry(identity.tid).unwrap().phase,
            Phase::Borrowed
        );
        next.parts().1.value = 29;
        next.complete().unwrap();
    });
    let scope = DispatchScratchScope::enter();
    let mut next = registry.acquire(identity, &scope.owner).unwrap();
    assert_eq!(next.parts().1.value, 29);
    registry.close(identity).unwrap();
    drop(next);
    assert!(super::super::drive_ready(registry.consume(identity.pid)).is_ok());
}

fn cleanup_stays_counted(available: bool, client_drop: bool) {
    let registry = registry();
    let root_scope = DispatchScratchScope::enter();
    let mut root = registry
        .reserve(
            Pid::from_raw(1),
            Pid::from_raw(1),
            client(1),
            &root_scope.owner,
            true,
        )
        .unwrap();
    root.initialize(None).unwrap();
    let entered = Arc::new(std::sync::Barrier::new(2));
    let resume = Arc::new(std::sync::Barrier::new(2));
    let drops = Arc::new(AtomicUsize::new(0));
    let weak = Arc::downgrade(&registry);
    let entered_drop = entered.clone();
    let resume_drop = resume.clone();
    let dropped = drops.clone();
    let hook: DropHook = Arc::new(move || {
        let registry = weak.upgrade().unwrap();
        assert_eq!(registry.retained_entries(), 2);
        let identity = registry
            .identity(Pid::from_raw(1), Pid::from_raw(2))
            .unwrap();
        let scope = DispatchScratchScope::enter();
        assert!(registry.acquire(identity, &scope.owner).is_err());
        assert!(registry.retire(identity).is_err());
        entered_drop.wait();
        resume_drop.wait();
        assert_eq!(registry.retained_entries(), 1);
        let mut finish = Box::pin(registry.consume(Pid::from_raw(1)));
        let notified = Arc::new(Notify(AtomicUsize::new(0)));
        let waker = std::task::Waker::from(notified);
        let mut context = std::task::Context::from_waker(&waker);
        assert!(std::future::Future::poll(finish.as_mut(), &mut context).is_pending());
        dropped.fetch_add(1, Ordering::SeqCst);
    });
    let child_client = Arc::new(Client(
        Pid::from_raw(1),
        Pid::from_raw(2),
        client_drop.then(|| hook.clone()),
    ));
    let child_scope = DispatchScratchScope::enter();
    let identity = root
        .construct_child(Pid::from_raw(2), child_client, &child_scope.owner)
        .unwrap();
    let mut child = registry.acquire(identity, &child_scope.owner).unwrap();
    if !client_drop {
        child.parts().1.on_drop = Some(hook);
    }
    child.complete().unwrap();
    std::thread::scope(|threads| {
        let registry_ref = &registry;
        let cleanup = threads.spawn(move || {
            let scope = DispatchScratchScope::enter();
            if available {
                registry_ref.retire(identity).unwrap();
            } else {
                let invocation = registry_ref.acquire(identity, &scope.owner).unwrap();
                registry_ref.retire(identity).unwrap();
                drop(invocation);
            }
        });
        entered.wait();
        assert_eq!(registry.retained_entries(), 2);
        registry.close(root.identity()).unwrap();
        drop(root);
        assert_eq!(registry.retained_entries(), 1);
        assert_eq!(registry.process.lock().leases, 1);
        let mut finish = Box::pin(registry.consume(Pid::from_raw(1)));
        let notified = Arc::new(Notify(AtomicUsize::new(0)));
        let waker = std::task::Waker::from(notified);
        let mut context = std::task::Context::from_waker(&waker);
        assert!(std::future::Future::poll(finish.as_mut(), &mut context).is_pending());
        assert!(registry.tool_present());
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        resume.wait();
        cleanup.join().unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(registry.retained_entries(), 0);
        assert_eq!(registry.process.lock().leases, 0);
        assert!(matches!(
            std::future::Future::poll(finish.as_mut(), &mut context),
            Poll::Ready(Ok(_))
        ));
    });
}

#[test]
fn available_state_destructor_blocks_consumption_and_remains_counted() {
    cleanup_stays_counted(true, false);
}

#[test]
fn available_client_destructor_blocks_consumption_and_remains_counted() {
    cleanup_stays_counted(true, true);
}

#[test]
fn borrowed_state_destructor_blocks_consumption_and_remains_counted() {
    cleanup_stays_counted(false, false);
}

#[test]
fn final_extracted_client_destructor_blocks_consumption_and_remains_counted() {
    cleanup_stays_counted(false, true);
}
