/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::MutexGuard;

use linked_hash_map::Entry;
use linked_hash_map::LinkedHashMap;

use super::library::Library;
use super::symbols::Symbols;

/// Maximum cache size in bytes, including all of the loaded debug info.
///
/// When the cache starts to exceed this size, the least recently used library's
/// symbols will be removed.
const DEFAULT_MAX_CACHE_SIZE: usize = 1 << 30; // 1 GiB

static CACHE: LazyLock<Mutex<DebugInfoCache>> = LazyLock::new(|| Mutex::new(DebugInfoCache::new()));

/// An LRU cache of loaded symbols. This is shared by all processes since we
/// only need to load symbols once for a particular inode. However, each process
/// will have mapped these libraries into memory differently, so that is tracked
/// on a per-process basis.
pub struct DebugInfoCache {
    /// Mapping of inode -> symbols.
    cache: LinkedHashMap<u64, Arc<Symbols>>,
    size: usize,
    max_size: usize,
}

impl DebugInfoCache {
    fn new() -> Self {
        Self {
            cache: Default::default(),
            size: 0,
            max_size: DEFAULT_MAX_CACHE_SIZE,
        }
    }

    /// The symbols already loaded for `library`, if any.
    fn get(&self, library: &Library) -> Option<Arc<Symbols>> {
        self.cache.get(&library.inode).cloned()
    }

    /// Records the symbols loaded for `library`, keeping an entry another
    /// thread recorded first. Returns the symbols to use.
    fn insert(&mut self, library: &Library, symbols: Arc<Symbols>) -> Arc<Symbols> {
        match self.cache.entry(library.inode) {
            Entry::Occupied(entry) => entry.get().clone(),
            Entry::Vacant(entry) => {
                self.size += symbols.bytes_used();

                let symbols = entry.insert(symbols).clone();

                // Evict older entries if we've exceeded the max cache size.
                // Even if we evict the entry we just inserted, that's fine
                // since we still return an `Arc`.
                while self.size > self.max_size {
                    if let Some((_k, v)) = self.cache.pop_front() {
                        self.size -= v.bytes_used();
                    }
                }

                symbols
            }
        }
    }
}

/// Loads the symbols for the given library. Returns an error if the symbols
/// failed to load.
///
/// The cache's lock is not held while the library's file is opened: the open
/// takes a `launch_window::TransientOpen`, which waits for another thread's
/// launch, and that launch's pthread_atfork prepare handlers could need this
/// lock. Two threads may load the same library at once; the first to finish
/// keeps its entry.
pub fn load(library: &Library) -> Result<Arc<Symbols>, anyhow::Error> {
    if let Some(symbols) = cache().get(library) {
        return Ok(symbols);
    }
    let symbols = Arc::new(Symbols::load(&library.path)?);
    Ok(cache().insert(library, symbols))
}

fn cache() -> MutexGuard<'static, DebugInfoCache> {
    CACHE.lock().expect("lock poisoned")
}
