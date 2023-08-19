use core::time::Duration;
#[cfg(feature = "std")]
use std::{
    mem::replace,
    sync::{Condvar, Mutex},
};

/// Elapsed time counter.
pub trait Instant {
    fn now() -> Self;
    fn elapsed(&self) -> Duration;
}

/// Binary semaphore.
pub trait Semaphore: Default {
    type Instant: Instant;

    /// Increment semaphore.
    ///
    /// Does nothing if already given.
    fn give(&self);

    /// Try decrement semaphore.
    ///
    /// Returns previous value.
    ///
    /// Does nothing if already taken.
    fn try_take(&self) -> bool;

    /// Wait for semaphore to be given and take it.
    ///
    /// Returns:
    /// + on success - `true`,
    /// + on timeout - `false`.
    fn take(&self, timeout: Option<Duration>) -> bool;
}

#[cfg(feature = "std")]
pub use std::time::Instant as StdInstant;

#[cfg(feature = "std")]
impl Instant for StdInstant {
    fn now() -> Self {
        StdInstant::now()
    }
    fn elapsed(&self) -> Duration {
        StdInstant::elapsed(self)
    }
}

#[cfg(feature = "std")]
#[derive(Default)]
pub struct StdSemaphore {
    condvar: Condvar,
    mutex: Mutex<bool>,
}

#[cfg(feature = "std")]
impl Semaphore for StdSemaphore {
    type Instant = StdInstant;

    fn give(&self) {
        let mut guard = self.mutex.lock().unwrap();
        *guard = true;
        self.condvar.notify_one();
    }

    fn try_take(&self) -> bool {
        replace(&mut self.mutex.lock().unwrap(), false)
    }
    fn take(&self, timeout: Option<Duration>) -> bool {
        let mut guard = self.mutex.lock().unwrap();
        for timeout in TimeoutIterator::<Self::Instant>::new(timeout) {
            if replace(&mut guard, false) {
                return true;
            }
            match timeout {
                Some(t) => {
                    let r;
                    (guard, r) = self.condvar.wait_timeout(guard, t).unwrap();
                    if r.timed_out() {
                        break;
                    }
                }
                None => guard = self.condvar.wait(guard).unwrap(),
            };
        }
        replace(&mut guard, false)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TimeoutIterator<I: Instant> {
    start: I,
    timeout: Option<Duration>,
}

impl<I: Instant> TimeoutIterator<I> {
    pub fn new(timeout: Option<Duration>) -> Self {
        Self { start: I::now(), timeout }
    }
}

impl<I: Instant> Iterator for TimeoutIterator<I> {
    type Item = Option<Duration>;
    fn next(&mut self) -> Option<Self::Item> {
        match self.timeout {
            Some(dur) => {
                let elapsed = self.start.elapsed();
                if dur > elapsed {
                    Some(Some(dur - elapsed))
                } else {
                    None
                }
            }
            None => Some(None),
        }
    }
}
