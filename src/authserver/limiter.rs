//! Failed-login throttling for the consent screen's password form.
//!
//! In memory, and it resets on restart. That is acceptable here and the reasoning is worth stating
//! rather than discovering later: there is exactly one account, the password is an Argon2id hash
//! rather than something guessable at any rate this allows, and the window is a minute. An attacker
//! who can restart the process to clear the counter already has the box. The alternative, a table of
//! attempts, adds a write to the login path and a row nobody reads, and it would still not stop a
//! distributed attempt, which is what the fixed delay and Argon2's cost are for.
//!
//! Two windows, per key and global. The key is chosen by `throttle_key` in `routes.rs`: the first
//! entry of `X-Forwarded-For` whenever the header is present, the peer address when it is absent,
//! and the string `unknown` when the server was built without `ConnectInfo`. No check is made of
//! who sent the header, so a client talking to this server directly can name any address it likes
//! and the per-key window will believe it. The header is trustworthy only behind a reverse proxy
//! that overwrites it with the connecting address (`deploy/Caddyfile` sets
//! `header_up X-Forwarded-For {remote_host}`), which is the deployment this server assumes. The
//! global window is what makes inventing an address pointless either way, since rotating a fake
//! address still spends the same global budget.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(60);

/// How much more the whole server may spend than any single caller. Small enough to cap a brute
/// force, large enough that one throttled address cannot lock the owner out from another.
const GLOBAL_MULTIPLIER: u32 = 4;

/// A ceiling on tracked keys, so a caller rotating a forwarded address cannot grow this map without
/// bound. Reaching it clears the per-key windows and leaves the global one, which is the window that
/// was doing the work in that scenario anyway.
const MAX_KEYS: usize = 4096;

pub struct LoginLimiter {
    per_minute: u32,
    state: Mutex<State>,
}

struct State {
    by_key: HashMap<String, Vec<Instant>>,
    global: Vec<Instant>,
}

impl LoginLimiter {
    pub fn new(per_minute: u32) -> Self {
        Self {
            // Zero would lock the owner out of their own server on the first typo. A configured zero
            // is read as one attempt per minute rather than none.
            per_minute: per_minute.max(1),
            state: Mutex::new(State { by_key: HashMap::new(), global: Vec::new() }),
        }
    }

    /// Whether a password check may run for this caller.
    pub fn allow(&self, key: &str, now: Instant) -> bool {
        let mut state = self.lock();
        prune(&mut state.global, now);
        if state.global.len() as u32 >= self.per_minute.saturating_mul(GLOBAL_MULTIPLIER) {
            return false;
        }
        match state.by_key.get_mut(key) {
            Some(window) => {
                prune(window, now);
                (window.len() as u32) < self.per_minute
            }
            None => true,
        }
    }

    /// Record a failure. Successes are not counted: the limit exists to slow guessing, and counting
    /// the owner's successful logins would throttle the owner for using their own server.
    pub fn record_failure(&self, key: &str, now: Instant) {
        let mut state = self.lock();
        state.global.push(now);
        prune(&mut state.global, now);

        if state.by_key.len() >= MAX_KEYS && !state.by_key.contains_key(key) {
            state.by_key.clear();
        }
        let window = state.by_key.entry(key.to_string()).or_default();
        window.push(now);
        prune(window, now);
    }

    /// A poisoned lock means a panic inside a previous call. The counters are not worth propagating
    /// that panic into the login path, so the poison is stepped over and the state kept.
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn prune(window: &mut Vec<Instant>, now: Instant) {
    window.retain(|at| now.duration_since(*at) < WINDOW);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_attempt_from_an_unseen_address_is_allowed() {
        let limiter = LoginLimiter::new(3);
        assert!(limiter.allow("1.2.3.4", Instant::now()));
    }

    #[test]
    fn attempts_are_refused_once_the_window_is_full() {
        let limiter = LoginLimiter::new(3);
        let now = Instant::now();
        for _ in 0..3 {
            assert!(limiter.allow("1.2.3.4", now));
            limiter.record_failure("1.2.3.4", now);
        }
        assert!(!limiter.allow("1.2.3.4", now));
    }

    #[test]
    fn a_full_window_ages_out_after_a_minute() {
        let limiter = LoginLimiter::new(2);
        let now = Instant::now();
        for _ in 0..2 {
            limiter.record_failure("1.2.3.4", now);
        }
        assert!(!limiter.allow("1.2.3.4", now));
        assert!(limiter.allow("1.2.3.4", now + Duration::from_secs(61)));
    }

    #[test]
    fn one_throttled_address_does_not_throttle_another() {
        let limiter = LoginLimiter::new(1);
        let now = Instant::now();
        limiter.record_failure("1.2.3.4", now);
        assert!(!limiter.allow("1.2.3.4", now));
        assert!(limiter.allow("5.6.7.8", now));
    }

    #[test]
    fn rotating_the_key_still_runs_into_the_global_window() {
        // The failure this exists for: a forwarded address is attacker-controlled, so a per-key
        // window alone can be bypassed by sending a different one each time.
        let limiter = LoginLimiter::new(1);
        let now = Instant::now();
        // per_minute is 1, so the global budget is GLOBAL_MULTIPLIER attempts.
        for i in 0..GLOBAL_MULTIPLIER {
            let key = format!("10.0.0.{i}");
            assert!(limiter.allow(&key, now), "attempt {i} should still be inside the budget");
            limiter.record_failure(&key, now);
        }
        assert!(!limiter.allow("10.0.0.99", now), "a fresh key must not reset the global window");
    }

    #[test]
    fn a_successful_login_does_not_count_against_the_owner() {
        let limiter = LoginLimiter::new(1);
        let now = Instant::now();
        assert!(limiter.allow("1.2.3.4", now));
        assert!(limiter.allow("1.2.3.4", now), "nothing was recorded, so nothing was spent");
    }

    #[test]
    fn a_configured_zero_does_not_lock_the_owner_out_entirely() {
        let limiter = LoginLimiter::new(0);
        assert!(limiter.allow("1.2.3.4", Instant::now()));
    }

    #[test]
    fn tracked_keys_stay_bounded() {
        let limiter = LoginLimiter::new(1);
        let now = Instant::now();
        for i in 0..(MAX_KEYS + 10) {
            limiter.record_failure(&format!("key-{i}"), now);
        }
        assert!(limiter.lock().by_key.len() <= MAX_KEYS);
    }
}
