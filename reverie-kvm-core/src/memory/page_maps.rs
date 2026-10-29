/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The page-keyed host-side access maps, with two properties the Tool-scratch
//! fast paths rely on.
//!
//! - [`Versioned`] collections change only through [`Versioned::edit`], which
//!   gives the collection a process-unique version. Two collections with the
//!   same version therefore have the same contents: the only way to share a
//!   version is to clone the collection that drew it. The inner collection is
//!   private to this module, so a mutation that skips the version does not
//!   compile.
//! - [`UserPages`] is logically a `BTreeMap<u64, UserPageState>`. Its
//!   representation adds a tiny overlay of exposed Tool-scratch pages, each
//!   logically `Accessible { writable: true }`, so exposing and hiding a
//!   scratch page touches no tree node. Every method answers for the logical
//!   map, and the overlay is never a key of the tree.
//! - [`UserPages`] can watch a range of pages ([`UserPages::watch`]): every
//!   method that changes the logical state of a watched page records it, so
//!   no caller can change one unseen. A clone shares the record, and two
//!   maps can be made to share one ([`UserPages::share_watch`]) until either
//!   takes one of its own ([`UserPages::detach_watch`]).

use std::collections::BTreeMap;
use std::fmt;
use std::ops::Bound;
use std::ops::Deref;
use std::ops::RangeBounds;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use super::UserPageState;

static NEXT_VERSION: AtomicU64 = AtomicU64::new(1);

fn fresh_version() -> u64 {
    NEXT_VERSION.fetch_add(1, Ordering::Relaxed)
}

/// A collection readable through `Deref` and writable only through
/// [`Versioned::edit`]. See the module documentation.
#[derive(Clone)]
pub(super) struct Versioned<M> {
    inner: M,
    version: u64,
}

impl<M> Versioned<M> {
    pub(super) fn new(inner: M) -> Self {
        Self {
            inner,
            version: fresh_version(),
        }
    }

    /// Mutable access. The collection takes a fresh version first, whether or
    /// not the caller ends up changing it, so a stale version can only be
    /// conservative.
    pub(super) fn edit(&mut self) -> &mut M {
        self.version = fresh_version();
        &mut self.inner
    }

    pub(super) fn version(&self) -> u64 {
        self.version
    }
}

impl<M> Deref for Versioned<M> {
    type Target = M;

    fn deref(&self) -> &M {
        &self.inner
    }
}

impl<'a, M> IntoIterator for &'a Versioned<M>
where
    &'a M: IntoIterator,
{
    type Item = <&'a M as IntoIterator>::Item;
    type IntoIter = <&'a M as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        (&self.inner).into_iter()
    }
}

impl<M: fmt::Debug> fmt::Debug for Versioned<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.fmt(f)
    }
}

/// The logical state of every exposed Tool-scratch page.
const EXPOSED: UserPageState = UserPageState::Accessible { writable: true };

/// The page-state map; see the module documentation.
#[derive(Clone)]
pub(super) struct UserPages {
    map: Versioned<BTreeMap<u64, UserPageState>>,
    /// Exposed Tool-scratch pages, ascending, never keys of `map`. One per
    /// Tool thread inside a callback, so a linear scan is the right search.
    scratch: Vec<u64>,
    /// The watched pages, if any; see [`UserPages::watch`].
    watch: Option<Watch>,
}

/// A range of pages whose logical state [`UserPages`] watches.
#[derive(Clone, Debug)]
struct Watch {
    /// The first watched page.
    first: u64,
    /// The last watched page.
    last: u64,
    /// Whether the logical state of a watched page has changed, or
    /// [`UserPages::note_change`] named one, since the watch began: in this
    /// map, or in any map that shares the record (a clone of this one, or
    /// one that [`UserPages::share_watch`] joined to it). It only ever goes
    /// from false to true.
    changed: Arc<AtomicBool>,
}

/// A watch ([`UserPages::watch`]) and its record, taken from one map for
/// another to share ([`UserPages::share_watch`]).
#[derive(Clone, Debug)]
pub(super) struct WatchRecord(Option<Watch>);

impl UserPages {
    pub(super) fn new() -> Self {
        Self {
            map: Versioned::new(BTreeMap::new()),
            scratch: Vec::new(),
            watch: None,
        }
    }

    /// Watches pages `first..=last` from now on, replacing any earlier watch:
    /// once a method of this map changes the logical state of one of them
    /// (inserts a page with another state, removes one, clears the map, or
    /// exposes or hides a scratch page over one), or
    /// [`UserPages::note_change`] names one, the watch records that it
    /// changed, until the next `watch`. The new watch has a record of its
    /// own, shared with no other map.
    ///
    /// A clone shares the record: a change either records is recorded for
    /// both, so a copy prepared from this map and installed in its place
    /// (or dropped) loses nothing. A copy that becomes another address space,
    /// a fork's, takes a record of its own ([`UserPages::detach_watch`]).
    pub(super) fn watch(&mut self, first: u64, last: u64) {
        self.watch = Some(Watch {
            first,
            last,
            changed: Arc::new(AtomicBool::new(false)),
        });
    }

    /// The watched pages, `(first, last)`, if a watch is set and none of its
    /// pages has changed since it began, in this map or in a map that shares
    /// its record.
    pub(super) fn unchanged_watch(&self) -> Option<(u64, u64)> {
        self.watch
            .as_ref()
            .filter(|watch| !watch.changed.load(Ordering::SeqCst))
            .map(|watch| (watch.first, watch.last))
    }

    /// Records that something the logical states do not hold changed for
    /// pages `first..=last`: when they overlap the watch, it has changed.
    pub(super) fn note_change(&mut self, first: u64, last: u64) {
        if let Some(watch) = &self.watch
            && first <= watch.last
            && last >= watch.first
        {
            watch.changed.store(true, Ordering::SeqCst);
        }
    }

    /// This map's watch and its record, for another map to share
    /// ([`UserPages::share_watch`]).
    pub(super) fn watch_record(&self) -> WatchRecord {
        WatchRecord(self.watch.clone())
    }

    /// Makes `record`'s watch this map's, replacing its own: from now on a
    /// change this map records is recorded for the map the record came from,
    /// and the reverse, as for two copies of one address space.
    pub(super) fn share_watch(&mut self, record: WatchRecord) {
        self.watch = record.0;
    }

    /// Gives this map's watch a record of its own, which starts with the
    /// answer the shared one has now: changes recorded by the maps that
    /// shared it no longer reach this one, and this one's no longer reach
    /// them.
    pub(super) fn detach_watch(&mut self) {
        if let Some(watch) = &mut self.watch {
            watch.changed = Arc::new(AtomicBool::new(watch.changed.load(Ordering::SeqCst)));
        }
    }

    /// The version of the tree. Overlay changes do not move it: they change
    /// no tree key.
    pub(super) fn map_version(&self) -> u64 {
        self.map.version()
    }

    fn scratch_index(&self, page: u64) -> Result<usize, usize> {
        self.scratch.binary_search(&page)
    }

    fn take_scratch(&mut self, page: u64) -> bool {
        match self.scratch_index(page) {
            Ok(index) => {
                self.scratch.remove(index);
                true
            }
            Err(_) => false,
        }
    }

    pub(super) fn get(&self, page: &u64) -> Option<&UserPageState> {
        if !self.scratch.is_empty() && self.scratch_index(*page).is_ok() {
            return Some(&EXPOSED);
        }
        self.map.get(page)
    }

    pub(super) fn contains_key(&self, page: &u64) -> bool {
        self.get(page).is_some()
    }

    pub(super) fn len(&self) -> usize {
        self.map.len() + self.scratch.len()
    }

    /// `BTreeMap::insert` of the logical map.
    pub(super) fn insert(&mut self, page: u64, state: UserPageState) -> Option<UserPageState> {
        let exposed = self.take_scratch(page);
        let previous = self.map.edit().insert(page, state);
        debug_assert!(!(exposed && previous.is_some()));
        let previous = if exposed { Some(EXPOSED) } else { previous };
        if previous != Some(state) {
            self.note_change(page, page);
        }
        previous
    }

    /// `BTreeMap::remove` of the logical map.
    pub(super) fn remove(&mut self, page: &u64) -> Option<UserPageState> {
        let previous = if self.take_scratch(*page) {
            Some(EXPOSED)
        } else {
            self.map.edit().remove(page)
        };
        if previous.is_some() {
            self.note_change(*page, *page);
        }
        previous
    }

    pub(super) fn clear(&mut self) {
        if let Some((first, last)) = self.watch.as_ref().map(|watch| (watch.first, watch.last))
            && self.range(first..=last).next().is_some()
        {
            self.note_change(first, last);
        }
        self.scratch.clear();
        self.map.edit().clear();
    }

    /// Exactly `insert(page, Accessible { writable: true })` of the logical
    /// map, without touching the tree when `page` is known not to be one of
    /// its keys. `known_absent` must be a fact, not a guess: with it the tree
    /// is not searched.
    pub(super) fn expose_scratch(&mut self, page: u64, known_absent: bool) {
        let Err(index) = self.scratch_index(page) else {
            // Already exposed: inserting the same state changes nothing.
            return;
        };
        let previous = if !known_absent && self.map.contains_key(&page) {
            self.map.edit().remove(&page)
        } else {
            None
        };
        self.scratch.insert(index, page);
        if previous != Some(EXPOSED) {
            self.note_change(page, page);
        }
    }

    /// Exactly `remove(&page)` of the logical map, without touching the tree
    /// when `page` is known not to be one of its keys.
    pub(super) fn hide_scratch(&mut self, page: u64, known_absent: bool) {
        if self.take_scratch(page) {
            self.note_change(page, page);
            return;
        }
        if known_absent {
            return;
        }
        if self.map.contains_key(&page) {
            self.map.edit().remove(&page);
            self.note_change(page, page);
        }
    }

    /// The logical entries with keys in `range`, ascending, double-ended.
    pub(super) fn range<R: RangeBounds<u64> + Clone>(&self, range: R) -> Iter<'_> {
        let tree = self.map.range(range.clone());
        let lo = self
            .scratch
            .partition_point(|page| match range.start_bound() {
                Bound::Included(start) => page < start,
                Bound::Excluded(start) => page <= start,
                Bound::Unbounded => false,
            });
        let hi = self
            .scratch
            .partition_point(|page| match range.end_bound() {
                Bound::Included(end) => page <= end,
                Bound::Excluded(end) => page < end,
                Bound::Unbounded => true,
            });
        Iter {
            tree,
            scratch: &self.scratch[lo..hi.max(lo)],
            front: None,
            back: None,
        }
    }

    pub(super) fn iter(&self) -> Iter<'_> {
        self.range(..)
    }

    pub(super) fn keys(&self) -> impl DoubleEndedIterator<Item = &u64> + '_ {
        self.iter().map(|(page, _)| page)
    }

    /// The logical map as an ordinary map.
    #[cfg(test)]
    pub(super) fn to_map(&self) -> BTreeMap<u64, UserPageState> {
        self.iter().map(|(&page, &state)| (page, state)).collect()
    }
}

impl fmt::Debug for UserPages {
    /// Formats as the logical `BTreeMap` would.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

/// A sorted merge of tree entries and overlay pages, which are disjoint.
pub(super) struct Iter<'a> {
    tree: std::collections::btree_map::Range<'a, u64, UserPageState>,
    scratch: &'a [u64],
    /// A tree entry taken from the front and not yet yielded.
    front: Option<(&'a u64, &'a UserPageState)>,
    /// A tree entry taken from the back and not yet yielded.
    back: Option<(&'a u64, &'a UserPageState)>,
}

impl<'a> Iterator for Iter<'a> {
    type Item = (&'a u64, &'a UserPageState);

    fn next(&mut self) -> Option<Self::Item> {
        if self.front.is_none() {
            // Once the tree is drained from the front, its smallest remaining
            // entry may be the one already taken from the back.
            self.front = self.tree.next().or_else(|| self.back.take());
        }
        match (self.front, self.scratch.first()) {
            (Some((page, _)), Some(exposed)) if exposed < page => {
                self.scratch = &self.scratch[1..];
                Some((exposed, &EXPOSED))
            }
            (Some(_), _) => self.front.take(),
            (None, Some(exposed)) => {
                self.scratch = &self.scratch[1..];
                Some((exposed, &EXPOSED))
            }
            (None, None) => None,
        }
    }
}

impl DoubleEndedIterator for Iter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.back.is_none() {
            self.back = self.tree.next_back().or_else(|| self.front.take());
        }
        match (self.back, self.scratch.last()) {
            (Some((page, _)), Some(exposed)) if exposed > page => {
                self.scratch = &self.scratch[..self.scratch.len() - 1];
                Some((exposed, &EXPOSED))
            }
            (Some(_), _) => self.back.take(),
            (None, Some(exposed)) => {
                self.scratch = &self.scratch[..self.scratch.len() - 1];
                Some((exposed, &EXPOSED))
            }
            (None, None) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small deterministic generator; the test needs coverage, not quality.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self, bound: u64) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 >> 33) % bound
        }
    }

    fn state(choice: u64) -> UserPageState {
        match choice % 3 {
            0 => UserPageState::NoAccess,
            1 => UserPageState::Accessible { writable: false },
            _ => UserPageState::Accessible { writable: true },
        }
    }

    fn assert_same(pages: &UserPages, model: &BTreeMap<u64, UserPageState>, rng: &mut Lcg) {
        assert_eq!(pages.len(), model.len());
        assert_eq!(&pages.to_map(), model);
        assert!(pages.keys().eq(model.keys()));
        assert!(pages.keys().rev().eq(model.keys().rev()));
        assert_eq!(format!("{pages:?}"), format!("{model:?}"));
        for page in 0..24 {
            assert_eq!(pages.get(&page), model.get(&page), "page {page}");
            assert_eq!(pages.contains_key(&page), model.contains_key(&page));
        }
        // Random bounded ranges, both directions, and interleaved ends.
        for _ in 0..4 {
            let start = rng.next(24);
            let end = start + rng.next(24 - start + 1);
            assert!(pages.range(start..end).eq(model.range(start..end)));
            assert!(
                pages
                    .range(start..=end)
                    .rev()
                    .eq(model.range(start..=end).rev())
            );
            let (mut ours, mut theirs) = (pages.range(start..), model.range(start..));
            loop {
                let back = rng.next(2) == 0;
                let (a, b) = if back {
                    (ours.next_back(), theirs.next_back())
                } else {
                    (ours.next(), theirs.next())
                };
                assert_eq!(a, b);
                if a.is_none() {
                    break;
                }
            }
        }
    }

    /// Model check: every operation, overlay paths included, leaves the same
    /// logical map as the plain `BTreeMap` operation it stands for, and the
    /// watch records a change exactly when an operation changed the logical
    /// state of a watched page, or `note_change` named one.
    #[test]
    fn user_pages_behave_as_their_logical_btree_map() {
        let mut rng = Lcg(0x5eed);
        let mut changes = 0;
        for _round in 0..64 {
            let mut pages = UserPages::new();
            let mut model = BTreeMap::new();
            assert_eq!(pages.unchanged_watch(), None);
            let first = rng.next(20);
            let last = first + rng.next(20 - first);
            let mut watching = false;
            let mut changed = false;
            for step in 0..200 {
                if step == 20 {
                    // A watch begun on a map that already holds pages.
                    pages.watch(first, last);
                    watching = true;
                    changed = false;
                }
                let page = rng.next(20);
                let before: Vec<_> = model
                    .range(first..=last)
                    .map(|(&page, &state)| (page, state))
                    .collect();
                let mut noted = false;
                match rng.next(8) {
                    0 | 1 => {
                        let value = state(rng.next(3));
                        assert_eq!(pages.insert(page, value), model.insert(page, value));
                    }
                    2 => assert_eq!(pages.remove(&page), model.remove(&page)),
                    3 => {
                        // `known_absent` only when it is true of the tree.
                        let absent = !pages.map.contains_key(&page) && rng.next(2) == 0;
                        pages.expose_scratch(page, absent);
                        model.insert(page, EXPOSED);
                    }
                    4 | 5 => {
                        let absent = !pages.map.contains_key(&page) && rng.next(2) == 0;
                        pages.hide_scratch(page, absent);
                        model.remove(&page);
                    }
                    6 => {
                        if rng.next(8) == 0 {
                            let end = page + rng.next(4);
                            pages.note_change(page, end);
                            noted = page <= last && end >= first;
                        }
                    }
                    _ => {
                        if rng.next(16) == 0 {
                            pages.clear();
                            model.clear();
                        }
                    }
                }
                let after: Vec<_> = model
                    .range(first..=last)
                    .map(|(&page, &state)| (page, state))
                    .collect();
                if watching && (noted || before != after) {
                    changed = true;
                    changes += 1;
                }
                let expected = (watching && !changed).then_some((first, last));
                assert_eq!(pages.unchanged_watch(), expected, "step {step}");
                assert!(
                    pages
                        .scratch
                        .iter()
                        .all(|page| !pages.map.contains_key(page))
                );
                assert!(pages.scratch.windows(2).all(|w| w[0] < w[1]));
                assert_same(&pages, &model, &mut rng);
                let cloned = pages.clone();
                assert_same(&cloned, &model, &mut rng);
                assert_eq!(cloned.unchanged_watch(), expected);
            }
        }
        // The rounds exercised the watch, not only its absence.
        assert!(changes > 64, "{changes}");
    }

    /// Who shares a watch's record: a clone does, and a map joined to it
    /// with `share_watch`, both ways, until one detaches, which keeps the
    /// answer it had; a new watch shares with no one.
    #[test]
    fn a_watch_record_is_shared_by_clones_and_joined_maps_until_detached() {
        let watched = || {
            let mut pages = UserPages::new();
            pages.insert(5, UserPageState::NoAccess);
            pages.watch(4, 6);
            pages
        };
        let unchanged = Some((4, 6));

        // A clone, changed after the cloning, changes the original, and the
        // original changes the clone.
        for clone_changes in [true, false] {
            let mut original = watched();
            let mut clone = original.clone();
            let changing = if clone_changes {
                &mut clone
            } else {
                &mut original
            };
            changing.remove(&5);
            assert_eq!(original.unchanged_watch(), None, "{clone_changes}");
            assert_eq!(clone.unchanged_watch(), None, "{clone_changes}");
        }

        // A detached copy keeps the answer it had and shares no change.
        for detach_copy in [true, false] {
            let mut original = watched();
            let mut copy = original.clone();
            if detach_copy {
                copy.detach_watch();
            } else {
                original.detach_watch();
            }
            copy.insert(6, UserPageState::NoAccess);
            assert_eq!(original.unchanged_watch(), unchanged, "{detach_copy}");
            assert_eq!(copy.unchanged_watch(), None, "{detach_copy}");
            original.remove(&5);
            assert_eq!(original.unchanged_watch(), None, "{detach_copy}");
        }
        let mut changed = watched();
        changed.note_change(4, 4);
        let mut copy = changed.clone();
        copy.detach_watch();
        assert_eq!(copy.unchanged_watch(), None);

        // A map joined to another's record shares its changes both ways,
        // and a new watch on either leaves the other's record alone.
        for joined_changes in [true, false] {
            let mut original = watched();
            let mut joined = original.clone();
            joined.detach_watch();
            joined.share_watch(original.watch_record());
            let changing = if joined_changes {
                &mut joined
            } else {
                &mut original
            };
            changing.note_change(6, 9);
            assert_eq!(original.unchanged_watch(), None, "{joined_changes}");
            assert_eq!(joined.unchanged_watch(), None, "{joined_changes}");
        }
        let mut original = watched();
        let mut joined = UserPages::new();
        joined.share_watch(original.watch_record());
        assert_eq!(joined.unchanged_watch(), unchanged);
        joined.watch(4, 6);
        joined.note_change(5, 5);
        assert_eq!(original.unchanged_watch(), unchanged);
        original.watch(4, 6);
        let shared = original.clone();
        original.watch(4, 6);
        original.note_change(4, 4);
        assert_eq!(shared.unchanged_watch(), unchanged);
        // Without a watch there is nothing to share or detach.
        let mut none = UserPages::new();
        none.detach_watch();
        none.share_watch(UserPages::new().watch_record());
        assert_eq!(none.unchanged_watch(), None);
    }

    #[test]
    fn scratch_overlay_moves_no_tree_version_and_tree_edits_do() {
        let mut pages = UserPages::new();
        pages.insert(3, UserPageState::NoAccess);
        let version = pages.map_version();
        pages.expose_scratch(9, true);
        pages.hide_scratch(9, true);
        pages.expose_scratch(9, false);
        pages.hide_scratch(9, false);
        assert_eq!(pages.map_version(), version, "no tree key changed");
        // Negative controls: exposing over a tree key, or any tree edit,
        // takes a fresh version.
        pages.expose_scratch(3, false);
        let after_expose = pages.map_version();
        assert_ne!(after_expose, version);
        assert_eq!(pages.get(&3), Some(&EXPOSED));
        pages.insert(4, UserPageState::NoAccess);
        assert_ne!(pages.map_version(), after_expose);
    }

    #[test]
    fn versions_are_shared_only_by_clones() {
        let mut first = Versioned::new(BTreeMap::<u64, u64>::new());
        let second = Versioned::new(BTreeMap::<u64, u64>::new());
        assert_ne!(first.version(), second.version());
        let clone = first.clone();
        assert_eq!(clone.version(), first.version());
        first.edit().insert(1, 1);
        assert_ne!(clone.version(), first.version());
        let before = first.version();
        // Even an edit that changes nothing moves the version.
        first.edit().remove(&7);
        assert_ne!(first.version(), before);
    }
}
