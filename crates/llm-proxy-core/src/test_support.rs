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
/// Must only be used while holding [`TestEnvLock`] to guarantee thread safety.
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
    /// by holding `TestEnvLock`).
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
    /// by holding `TestEnvLock`).
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
