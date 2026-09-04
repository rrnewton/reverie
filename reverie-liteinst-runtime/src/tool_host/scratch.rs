use std::sync::Arc;

use super::SpinMutex;

#[derive(Clone)]
pub(super) struct ScratchOwner(Arc<SpinMutex<Retained>>);

struct Retained {
    open: bool,
    head: Option<Box<Arena>>,
}

struct Arena {
    bytes: Box<[u8]>,
    next: Option<Box<Arena>>,
}

impl ScratchOwner {
    pub(super) fn new() -> Self {
        Self(Arc::new(SpinMutex::new(Retained {
            open: true,
            head: None,
        })))
    }

    pub(super) fn retain(&self, bytes: Box<[u8]>) {
        let _allocation = crate::patch_alloc::enter_dispatch();
        if !self.0.lock().open {
            drop(bytes);
            return;
        }
        let mut arena = Box::new(Arena { bytes, next: None });
        let mut retained = self.0.lock();
        if retained.open {
            arena.next = retained.head.take();
            retained.head = Some(arena);
        } else {
            drop(retained);
            drop(arena);
        }
    }

    pub(super) fn close(&self) {
        let mut head = {
            let mut retained = self.0.lock();
            if !retained.open {
                return;
            }
            retained.open = false;
            retained.head.take()
        };
        #[cfg(feature = "test-tool-host-dispatch")]
        let mut arenas = 0;
        while let Some(mut arena) = head {
            head = arena.next.take();
            drop(arena.bytes);
            #[cfg(feature = "test-tool-host-dispatch")]
            {
                arenas += 1;
            }
        }
        #[cfg(feature = "test-tool-host-dispatch")]
        super::dispatch_observer::record(super::dispatch_observer::Event::ScratchRelease {
            arenas,
        });
    }
}

#[cfg(test)]
mod tests;
