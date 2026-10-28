//! Three-protocol source arbitration.
//!
//! One virtual receiver plays exactly one primary source at a time. A new
//! source *takes over*: the previous source is suspended (its protocol session
//! is kept alive), queued PCM is flushed, and only the new lease's audio is
//! forwarded to the engine. This mirrors the upstream `PlaybackArbiter` idea
//! but is owned by the fnOS host so MiPlay can join the same arbitration.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    AirPlay,
    Dlna,
    MiPlay,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::AirPlay => "airplay",
            Source::Dlna => "dlna",
            Source::MiPlay => "miplay",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lease {
    pub source: Source,
    pub epoch: u64,
}

pub type SuspendHook = Arc<dyn Fn(Lease) + Send + Sync>;

#[derive(Default)]
struct Inner {
    current: Option<Lease>,
    epoch: u64,
    last_tag: HashMap<Source, String>,
}

pub struct Arbiter {
    inner: Mutex<Inner>,
    hooks: Mutex<HashMap<Source, SuspendHook>>,
}

impl Default for Arbiter {
    fn default() -> Self {
        Self::new()
    }
}

/// Outcome of a `claim` call.
pub struct Claim {
    pub lease: Lease,
    /// Source that was playing before and is now suspended (if different).
    pub suspended: Option<Lease>,
}

impl Arbiter {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            hooks: Mutex::new(HashMap::new()),
        }
    }

    pub fn set_suspend_hook(&self, source: Source, hook: SuspendHook) {
        self.hooks.lock().unwrap().insert(source, hook);
    }

    pub fn current(&self) -> Option<Lease> {
        self.inner.lock().unwrap().current
    }

    pub fn current_source(&self) -> Option<Source> {
        self.current().map(|lease| lease.source)
    }

    /// Claim playback for `source`. Returns the new lease and the suspended
    /// previous lease (when a takeover actually happened). Hooks are called
    /// outside the lock; a hook must not call back into the arbiter.
    pub fn claim(&self, source: Source, tag: impl Into<String>) -> Claim {
        let suspended;
        let lease;
        {
            let mut inner = self.inner.lock().unwrap();
            inner.epoch += 1;
            lease = Lease { source, epoch: inner.epoch };
            suspended = match inner.current {
                Some(previous) if previous.source != source => Some(previous),
                _ => None,
            };
            inner.current = Some(lease);
            inner.last_tag.insert(source, tag.into());
        }
        if let Some(previous) = suspended {
            let hook = self.hooks.lock().unwrap().get(&previous.source).cloned();
            if let Some(hook) = hook {
                hook(previous);
            }
        }
        Claim { lease, suspended }
    }

    /// Release the lease if it is still current (source stopped/session ended).
    pub fn release(&self, lease: Lease) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if inner.current == Some(lease) {
            inner.current = None;
            true
        } else {
            false
        }
    }

    /// Whether PCM for this lease should be forwarded to the output engine.
    pub fn accepts(&self, lease: Lease) -> bool {
        self.inner.lock().unwrap().current == Some(lease)
    }

    pub fn last_tag(&self, source: Source) -> Option<String> {
        self.inner.lock().unwrap().last_tag.get(&source).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takeover_suspends_previous_source() {
        let arbiter = Arbiter::new();
        let suspended = Arc::new(Mutex::new(Vec::<Lease>::new()));
        let record = Arc::clone(&suspended);
        arbiter.set_suspend_hook(
            Source::AirPlay,
            Arc::new(move |lease| record.lock().unwrap().push(lease)),
        );
        let first = arbiter.claim(Source::AirPlay, "airplay_session");
        assert!(arbiter.accepts(first.lease));
        assert!(first.suspended.is_none());

        let second = arbiter.claim(Source::Dlna, "dlna_uri");
        assert!(second.lease.epoch > first.lease.epoch);
        assert_eq!(second.suspended.map(|lease| lease.source), Some(Source::AirPlay));
        assert_eq!(suspended.lock().unwrap().len(), 1);
        assert!(!arbiter.accepts(first.lease), "old lease must not reach the engine");
        assert!(arbiter.accepts(second.lease));
        assert_eq!(arbiter.current_source(), Some(Source::Dlna));

        // Same source reclaiming (e.g. next track) does not suspend itself.
        let third = arbiter.claim(Source::Dlna, "dlna_next");
        assert!(third.suspended.is_none());
        assert!(arbiter.accepts(third.lease));
    }

    #[test]
    fn release_only_affects_current_lease() {
        let arbiter = Arbiter::new();
        let lease = arbiter.claim(Source::MiPlay, "miplay").lease;
        let other = Lease { source: Source::AirPlay, epoch: 999 };
        assert!(!arbiter.release(other));
        assert!(arbiter.release(lease));
        assert_eq!(arbiter.current(), None);
    }
}
