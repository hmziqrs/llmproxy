//! Shared test utilities for `llm-proxy-core`.
//!
//! Provides a single crate-level mutex (`TEST_ENV_LOCK`) that serialises all
//! environment-variable-mutating tests across modules (currently used by
//! [`crate::env_interpolate`] and [`crate::provider_config`]), eliminating
//! the race condition that occurred when each module used its own independent
//! mutex.

use std::sync::Mutex;

/// Crate-level mutex for serialising tests that mutate environment variables.
///
/// Both `config::tests` and `provider_config::tests` acquire this lock before
/// setting or removing env vars, preventing the race condition where one
/// module's `EnvVarGuard` removes a variable that the other module's test
/// expects to be present.
///
/// # Safety invariant
///
/// The mutex guarantees exclusive access, so `unsafe { std::env::set_var(...) }`
/// and `unsafe { std::env::remove_var(...) }` calls within the lock scope are
/// safe: no other test thread can concurrently read or write the same variable.
pub static TEST_ENV_LOCK: Mutex<()> = Mutex::new(());

/// RAII wrapper that holds the test env mutex lock.
///
/// Acquire via [`TestEnvLock::acquire`]. The lock is released when this value
/// is dropped. This is a test utility for serialising environment-variable-mutating
/// tests to prevent race conditions.
#[allow(dead_code)]
#[derive(Debug)]
pub struct TestEnvLock {
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl TestEnvLock {
    /// Acquire the crate-level test env mutex, recovering from poison.
    pub fn acquire() -> Self {
        let guard = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        Self { _guard: guard }
    }
}

/// RAII guard that saves an environment variable on creation and restores it
/// (or removes it) on drop.
///
/// This is a test utility for safely setting/removing environment variables
/// in unit tests. It ensures the variable is always restored to its original
/// state when the guard is dropped, even if the test panics.
///
/// # Lock discipline (load-bearing invariant)
///
/// Both [`EnvVarGuard::set`] and [`EnvVarGuard::remove`] perform `unsafe`
/// `std::env::set_var` / `remove_var` calls, and [`Drop`] restores the
/// original value with the same `unsafe` calls. These are only sound while
/// no other thread is touching the process environment, i.e. while a
/// [`TestEnvLock`] is held.
///
/// `EnvVarGuard` is returned by value with **no lifetime tie** to the lock,
/// so this invariant is enforced by *convention*, not by the borrow checker.
/// Every existing call site respects it by binding the lock and the guard in
/// the same scope:
///
/// ```no_run
/// # use llm_proxy_core::test_support::{TestEnvLock, EnvVarGuard};
/// let _lock = TestEnvLock::acquire();        // acquired first ...
/// let _guard = EnvVarGuard::set("K", "v");   // ... so the guard is dropped
///                                             // *before* the lock (reverse
///                                             // drop order), keeping the
///                                             // restore inside the lock scope.
/// ```
///
/// **Callers must not let an `EnvVarGuard` outlive the `TestEnvLock` that
/// authorises it** (e.g. by returning it from a helper, storing it in a
/// struct, or moving it into a `'static` context). Doing so would make the
/// restore-on-drop run unsynchronised against concurrent env access. If such
/// a use case is needed, restructure the guard to own the `MutexGuard` (see
/// the TODO in `Drop` below) so the compiler enforces the invariant.
#[allow(dead_code)]
#[derive(Debug)]
pub struct EnvVarGuard {
    key: String,
    original: Option<String>,
}

impl EnvVarGuard {
    /// Set an environment variable, saving the previous value for restoration.
    ///
    /// # Safety
    ///
    /// Caller must ensure exclusive access to the process environment (e.g.
    /// by holding `TestEnvLock`), and must drop the returned guard before
    /// that lock is released (reverse drop order). See the type-level
    /// "Lock discipline" note.
    pub fn set(key: &str, value: &str) -> Self {
        let original = std::env::var(key).ok();
        unsafe {
            std::env::set_var(key, value);
        }
        Self {
            key: key.to_owned(),
            original,
        }
    }

    /// Remove an environment variable, saving the previous value for restoration.
    ///
    /// # Safety
    ///
    /// Caller must ensure exclusive access to the process environment (e.g.
    /// by holding `TestEnvLock`), and must drop the returned guard before
    /// that lock is released (reverse drop order). See the type-level
    /// "Lock discipline" note.
    pub fn remove(key: &str) -> Self {
        let original = std::env::var(key).ok();
        unsafe {
            std::env::remove_var(key);
        }
        Self {
            key: key.to_owned(),
            original,
        }
    }
}

/// Restore the saved environment variable.
///
/// # Safety
///
/// `Drop` performs the same `unsafe` `set_var` / `remove_var` as the
/// constructors and therefore inherits their contract: it must run while a
/// [`TestEnvLock`] is still held. This is guaranteed today solely by Rust's
/// reverse field-drop order — the guard is always dropped before the lock
/// binding it shares a scope with — because [`EnvVarGuard`] carries no
/// lifetime tie to the lock (see the type-level "Lock discipline" note).
///
/// TODO(test-support): for full defence-in-depth, restructure `EnvVarGuard`
/// to *own* the `MutexGuard` (have `set`/`remove` consume a `TestEnvLock`),
/// so the compiler proves the restore happens inside the lock. That change
/// requires updating every call site in `env_interpolate` and
/// `provider_config` tests and is deferred to a dedicated refactor.
impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(val) => unsafe {
                std::env::set_var(&self.key, val);
            },
            None => unsafe {
                std::env::remove_var(&self.key);
            },
        }
    }
}
