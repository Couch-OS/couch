//! Locks that know their place in the order, and say so when it is broken.
//!
//! The rules are in `docs/development/confd-locking.md`. This is what holds
//! the daemon to them: every lock of the hot set is a [`RankedMutex`] or a
//! [`RankedRwLock`] of one [`Level`], and in a debug build (which is what
//! `cargo test` builds) each thread keeps the list of levels it is holding.
//!
//! - **Waiting for a lock** whose rank is not higher than everything the
//!   thread already holds panics, there and then, on the first run of the
//!   code path - no second thread, no timing, no hang. Two threads can only
//!   wait on each other for good if one of them takes two locks in the wrong
//!   order, and that one acquisition is what is caught.
//! - **`try_lock`, `try_read` and `try_write` are exempt.** They never wait,
//!   so they cannot be one half of a deadlock, and the daemon leans on that:
//!   deleting a connection tries the settings locks while it holds the gate,
//!   a sweep tries a session while it holds the pairing map. What they take
//!   is still recorded, so a later wait is measured against it.
//! - **[`calling_out`]** marks the start of something slow that belongs to
//!   somebody else (a package child is asked something, a process is
//!   started). It panics if the thread holds a lock whose level says nothing
//!   slow may happen under it.
//!
//! In a release build all of it is gone: the types are a `Mutex` or an
//! `RwLock` and a zero-sized marker, `lock` is the inner `lock` and returns
//! the standard guard, and [`calling_out`] is an empty inline function.

use std::marker::PhantomData;
use std::sync::{Mutex, RwLock};

/// One place in the order. A lock may only be **waited for** while every lock
/// the thread holds is of a lower rank.
///
/// Nothing reads any of this in a release build, which is the point.
#[cfg_attr(not(debug_assertions), allow(dead_code))]
pub trait Level {
    const RANK: u8;
    /// What the panic calls it.
    const NAME: &'static str;
    /// Whether a package child may be asked something, or started, while a
    /// lock of this level is held. False for every lock a request handler
    /// needs in passing: a registry, a cache, the configuration.
    const CALL_OUT: bool;
}

/// The order, lowest first. The numbers leave room; a new lock takes a number
/// between the two it sits between, here and in the document.
pub mod level {
    use super::Level;

    macro_rules! levels {
        ($($(#[$doc:meta])* $name:ident = $rank:literal, $label:literal, call_out: $call_out:literal;)*) => {$(
            $(#[$doc])*
            pub struct $name;
            impl Level for $name {
                const RANK: u8 = $rank;
                const NAME: &'static str = $label;
                const CALL_OUT: bool = $call_out;
            }
        )*};
    }

    levels! {
        /// `gate_for`: read for the whole of every request under one
        /// connection, written (by `try_write` only) while it is deleted.
        ConnectionGate = 10, "connection gate", call_out: true;
        /// One pairing conversation. Held across the package's answer, which
        /// is why every request path only ever tries it.
        PairingSession = 20, "pairing session", call_out: true;
        /// `lock_for`: one connection's settings file, held for a whole
        /// request to its package. Only ever tried.
        ConnectionSettings = 30, "connection settings lock", call_out: true;
        /// `Api::store`: the configuration.
        ConfigStore = 40, "configuration store", call_out: false;
        /// `Runtime::catalog_generations`: the one-second tick's memory of
        /// which manifests it has already copied into the configuration.
        CatalogGenerations = 50, "catalog generations", call_out: false;
        /// `Runtime::endpoints`: the running package children.
        Endpoints = 60, "package registry", call_out: false;
        /// `Runtime::children`: the listing cache.
        Children = 70, "children listing cache", call_out: false;
        /// `Runtime::pairings`: which pairing each connection is having.
        Pairings = 80, "pairing map", call_out: false;
        /// `Runtime::paired_at`.
        PairedAt = 90, "pairing times", call_out: false;
        /// `Runtime::unreadable_keys`.
        UnreadableKeys = 100, "unreadable key notes", call_out: false;
    }
}

/// A `Mutex` of one [`Level`]. Same methods, same poisoning, same results.
pub struct RankedMutex<L: Level, T> {
    inner: Mutex<T>,
    level: PhantomData<fn() -> L>,
}

/// An `RwLock` of one [`Level`]. Reading and writing are the same rank.
pub struct RankedRwLock<L: Level, T> {
    inner: RwLock<T>,
    level: PhantomData<fn() -> L>,
}

impl<L: Level, T> RankedMutex<L, T> {
    pub const fn new(value: T) -> Self {
        Self {
            inner: Mutex::new(value),
            level: PhantomData,
        }
    }
}

impl<L: Level, T> RankedRwLock<L, T> {
    pub const fn new(value: T) -> Self {
        Self {
            inner: RwLock::new(value),
            level: PhantomData,
        }
    }
}

#[cfg(not(debug_assertions))]
mod plain {
    use super::{Level, RankedMutex, RankedRwLock};
    use std::sync::{LockResult, MutexGuard, RwLockReadGuard, RwLockWriteGuard, TryLockResult};

    impl<L: Level, T> RankedMutex<L, T> {
        #[inline(always)]
        pub fn lock(&self) -> LockResult<MutexGuard<'_, T>> {
            self.inner.lock()
        }
        #[inline(always)]
        pub fn try_lock(&self) -> TryLockResult<MutexGuard<'_, T>> {
            self.inner.try_lock()
        }
    }

    // The daemon reads the gate and tries to write it; the rest is here so
    // the type is an `RwLock` to whoever uses it next.
    #[allow(dead_code)]
    impl<L: Level, T> RankedRwLock<L, T> {
        #[inline(always)]
        pub fn read(&self) -> LockResult<RwLockReadGuard<'_, T>> {
            self.inner.read()
        }
        #[inline(always)]
        pub fn write(&self) -> LockResult<RwLockWriteGuard<'_, T>> {
            self.inner.write()
        }
        #[inline(always)]
        pub fn try_read(&self) -> TryLockResult<RwLockReadGuard<'_, T>> {
            self.inner.try_read()
        }
        #[inline(always)]
        pub fn try_write(&self) -> TryLockResult<RwLockWriteGuard<'_, T>> {
            self.inner.try_write()
        }
    }

    /// Nothing, in a release build.
    #[inline(always)]
    pub fn calling_out(_what: &str) {}
}
#[cfg(not(debug_assertions))]
pub use plain::calling_out;

#[cfg(debug_assertions)]
mod checked {
    use super::{Level, RankedMutex, RankedRwLock};
    use std::cell::RefCell;
    use std::ops::{Deref, DerefMut};
    use std::sync::{LockResult, PoisonError, TryLockError, TryLockResult};

    #[derive(Clone, Copy)]
    struct Held {
        rank: u8,
        name: &'static str,
        call_out: bool,
    }

    thread_local! {
        static HELD: RefCell<Vec<Held>> = const { RefCell::new(Vec::new()) };
    }

    const SEE: &str = "see docs/development/confd-locking.md";

    /// Before waiting for a lock of level `L`: everything held has to rank
    /// below it. Two locks of one level count as out of order too, because
    /// nothing says which of them comes first.
    fn about_to_wait<L: Level>() {
        let above = HELD
            .try_with(|held| held.borrow().iter().copied().find(|h| h.rank >= L::RANK))
            .ok()
            .flatten();
        if let Some(above) = above {
            panic!(
                "lock order: waiting for the {} (rank {}) while holding the {} (rank {}); {SEE}",
                L::NAME,
                L::RANK,
                above.name,
                above.rank,
            );
        }
    }

    fn taken<L: Level>() -> Token {
        let _ = HELD.try_with(|held| {
            held.borrow_mut().push(Held {
                rank: L::RANK,
                name: L::NAME,
                call_out: L::CALL_OUT,
            })
        });
        Token(L::RANK)
    }

    /// Guards are dropped in any order, so this takes out the most recent
    /// entry of its own rank rather than the top of a stack.
    struct Token(u8);
    impl Drop for Token {
        fn drop(&mut self) {
            let _ = HELD.try_with(|held| {
                let mut held = held.borrow_mut();
                if let Some(at) = held.iter().rposition(|h| h.rank == self.0) {
                    held.remove(at);
                }
            });
        }
    }

    /// Something slow that is somebody else's is about to start: a package
    /// child is asked something, a process is started. Panics if this thread
    /// holds a lock that nothing slow may happen under.
    pub fn calling_out(what: &str) {
        let under = HELD
            .try_with(|held| held.borrow().iter().copied().find(|h| !h.call_out))
            .ok()
            .flatten();
        if let Some(under) = under {
            panic!(
                "lock order: {what} while holding the {} (rank {}); {SEE}",
                under.name, under.rank,
            );
        }
    }

    /// What a ranked lock hands out: the standard guard, and this thread's
    /// note that it is held. The note goes first when it is dropped.
    pub struct Guard<G> {
        _held: Token,
        inner: G,
    }
    impl<G: Deref> Deref for Guard<G> {
        type Target = G::Target;
        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }
    impl<G: DerefMut> DerefMut for Guard<G> {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.inner
        }
    }

    fn waited<L: Level, G>(result: LockResult<G>) -> LockResult<Guard<G>> {
        let wrap = |inner| Guard {
            _held: taken::<L>(),
            inner,
        };
        match result {
            Ok(guard) => Ok(wrap(guard)),
            Err(poisoned) => Err(PoisonError::new(wrap(poisoned.into_inner()))),
        }
    }

    fn tried<L: Level, G>(result: TryLockResult<G>) -> TryLockResult<Guard<G>> {
        match result {
            Err(TryLockError::WouldBlock) => Err(TryLockError::WouldBlock),
            Ok(guard) => waited::<L, G>(Ok(guard)).map_err(TryLockError::Poisoned),
            Err(TryLockError::Poisoned(poisoned)) => {
                waited::<L, G>(Err(poisoned)).map_err(TryLockError::Poisoned)
            }
        }
    }

    impl<L: Level, T> RankedMutex<L, T> {
        pub fn lock(&self) -> LockResult<Guard<std::sync::MutexGuard<'_, T>>> {
            about_to_wait::<L>();
            waited::<L, _>(self.inner.lock())
        }
        pub fn try_lock(&self) -> TryLockResult<Guard<std::sync::MutexGuard<'_, T>>> {
            tried::<L, _>(self.inner.try_lock())
        }
    }

    // The daemon reads the gate and tries to write it; the rest is here so
    // the type is an `RwLock` to whoever uses it next.
    #[allow(dead_code)]
    impl<L: Level, T> RankedRwLock<L, T> {
        pub fn read(&self) -> LockResult<Guard<std::sync::RwLockReadGuard<'_, T>>> {
            about_to_wait::<L>();
            waited::<L, _>(self.inner.read())
        }
        pub fn write(&self) -> LockResult<Guard<std::sync::RwLockWriteGuard<'_, T>>> {
            about_to_wait::<L>();
            waited::<L, _>(self.inner.write())
        }
        pub fn try_read(&self) -> TryLockResult<Guard<std::sync::RwLockReadGuard<'_, T>>> {
            tried::<L, _>(self.inner.try_read())
        }
        pub fn try_write(&self) -> TryLockResult<Guard<std::sync::RwLockWriteGuard<'_, T>>> {
            tried::<L, _>(self.inner.try_write())
        }
    }
}
#[cfg(debug_assertions)]
pub use checked::calling_out;

#[cfg(all(test, debug_assertions))]
mod tests {
    use super::level::{ConfigStore, ConnectionGate, ConnectionSettings, Endpoints, Pairings};
    use super::*;

    fn panics(f: impl FnOnce()) -> String {
        let error = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
            .expect_err("it should have panicked");
        error.downcast_ref::<String>().cloned().unwrap_or_default()
    }

    #[test]
    fn locks_taken_in_order_are_taken_and_given_back_in_any_order() {
        let gate = RankedRwLock::<ConnectionGate, ()>::new(());
        let store = RankedMutex::<ConfigStore, u32>::new(1);
        let registry = RankedMutex::<Endpoints, u32>::new(2);
        let reading = gate.read().unwrap();
        let mut config = store.lock().unwrap();
        let children = registry.lock().unwrap();
        *config += *children;
        // Given back out of order, and then taken again in order.
        drop(config);
        drop(reading);
        drop(children);
        assert_eq!(*store.lock().unwrap(), 3);
        let _registry = registry.lock().unwrap();
    }

    #[test]
    fn waiting_for_a_lower_lock_while_holding_a_higher_one_panics_at_once() {
        let store = RankedMutex::<ConfigStore, ()>::new(());
        let registry = RankedMutex::<Endpoints, ()>::new(());
        let said = panics(|| {
            let _registry = registry.lock().unwrap();
            let _config = store.lock().unwrap();
        });
        assert_eq!(
            said,
            "lock order: waiting for the configuration store (rank 40) while holding the \
             package registry (rank 60); see docs/development/confd-locking.md"
        );
        // Nothing is left behind on this thread by the panic.
        let _config = store.lock().unwrap_or_else(|e| e.into_inner());
        let _registry = registry.lock().unwrap_or_else(|e| e.into_inner());
    }

    #[test]
    fn two_locks_of_one_level_may_be_tried_together_and_never_waited_for_together() {
        let (one, other) = (
            RankedMutex::<ConnectionSettings, ()>::new(()),
            RankedMutex::<ConnectionSettings, ()>::new(()),
        );
        {
            let _one = one.try_lock().unwrap();
            let _other = other.try_lock().unwrap();
        }
        let said = panics(|| {
            let _one = one.lock().unwrap();
            let _other = other.lock().unwrap();
        });
        assert!(said.contains("connection settings lock"), "{said}");
    }

    #[test]
    fn a_try_is_never_out_of_order_and_what_it_took_still_counts() {
        let pairings = RankedMutex::<Pairings, ()>::new(());
        let store = RankedMutex::<ConfigStore, ()>::new(());
        let held = pairings.lock().unwrap();
        // Lower, but only tried: this cannot wait, so it cannot deadlock.
        let config = store.try_lock().unwrap();
        assert!(matches!(
            store.try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        ));
        drop(held);
        // The store is held now, however it was come by.
        let said = panics(|| {
            let _config = config;
            let settings = RankedMutex::<ConnectionSettings, ()>::new(());
            let _settings = settings.lock().unwrap();
        });
        assert!(
            said.contains("while holding the configuration store"),
            "{said}"
        );
    }

    #[test]
    fn nothing_slow_starts_under_a_registry_and_anything_may_under_a_settings_lock() {
        let settings = RankedMutex::<ConnectionSettings, ()>::new(());
        let registry = RankedMutex::<Endpoints, ()>::new(());
        calling_out("nothing held");
        {
            let _settings = settings.try_lock().unwrap();
            calling_out("a request to a package");
        }
        let said = panics(|| {
            let _registry = registry.lock().unwrap();
            calling_out("a request to a package");
        });
        assert_eq!(
            said,
            "lock order: a request to a package while holding the package registry (rank 60); \
             see docs/development/confd-locking.md"
        );
        calling_out("given back");
    }

    #[test]
    fn a_poisoned_lock_is_still_handed_over_and_still_counted() {
        let store = std::sync::Arc::new(RankedMutex::<ConfigStore, u32>::new(7));
        let breaking = store.clone();
        let _ = std::thread::spawn(move || {
            let _held = breaking.lock().unwrap();
            panic!("poison it");
        })
        .join();
        let config = store.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(*config, 7);
        let said = panics(|| {
            let _config = config;
            calling_out("a request to a package");
        });
        assert!(said.contains("configuration store"), "{said}");
    }
}
