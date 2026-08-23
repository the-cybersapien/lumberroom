//! Failed-login throttling for the consent screen's password form, and the bound on how much
//! Argon2 work the two login paths may run at once.
//!
//! In memory, and it resets on restart. That is acceptable here and the reasoning is worth stating
//! rather than discovering later: there is exactly one account, the password is an Argon2id hash
//! rather than something guessable at any rate this allows, and the window is a minute. An attacker
//! who can restart the process to clear the counter already has the box. The alternative, a table of
//! attempts, adds a write to the login path and a row nobody reads, and it would still not stop a
//! distributed attempt, which is what the fixed delay and Argon2's cost are for.
//!
//! **The budget is taken before the password check, not after it.** An earlier build read the
//! window, ran Argon2id, waited out the failure delay, and only then recorded the attempt, so every
//! request that arrived inside that 800 ms saw an empty budget. A few hundred concurrent posts to
//! `/console/login` passed a ceiling of five and put a few gigabytes of Argon2 memory on the
//! blocking pool at once. `reserve` takes the slot and reads the windows in one mutex section;
//! a login that succeeds calls `release`, so the owner is not throttled for getting it right, and a
//! login that fails drops the guard, which leaves the attempt on the budget.
//!
//! Two windows, per key and per peer. The key is chosen by `throttle_key` in `routes.rs`: the first
//! entry of `X-Forwarded-For` whenever the header is present, the peer address when it is absent,
//! and the string `unknown` when the server was built without `ConnectInfo`. No check is made of
//! who sent the header, so a client talking to this server directly can name any address it likes
//! and the per-key window will believe it. The header is trustworthy only behind a reverse proxy
//! that overwrites it with the connecting address (`deploy/Caddyfile` sets
//! `header_up X-Forwarded-For {remote_host}`), which is the deployment this server assumes.
//!
//! The second window is keyed on the socket the connection came from, which nobody can forge, and
//! that is what makes inventing a forwarded address pointless: rotating the header still spends the
//! same peer budget. It was a single server-wide counter until it became clear what that cost. Four
//! spoofed addresses filled it, and from then on every caller was refused, including the owner
//! typing the right password from somewhere else. Keying it on the peer confines a direct flood to
//! the address it came from. Behind a proxy every request shares one peer, so the peer window is
//! shared there too and the per-key window is what separates callers; that is the deployment where
//! the forwarded address is trustworthy in the first place.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tokio::sync::{Semaphore, SemaphorePermit};

const WINDOW: Duration = Duration::from_secs(60);

/// How much more one peer address may spend than any single throttle key it names. Small enough to
/// cap a brute force, large enough that a proxy carrying a few callers is not a single budget.
const PEER_MULTIPLIER: u32 = 4;

/// A ceiling on tracked keys, so a caller rotating a forwarded address cannot grow this map without
/// bound. Reaching it clears the per-key windows and leaves the peer ones, which is the window that
/// was doing the work in that scenario anyway.
const MAX_KEYS: usize = 4096;

/// How many Argon2id verifications may run at once across every login surface.
///
/// The limiter alone does not bound this. A throttle key is whatever the caller says it is, so N
/// distinct keys buy N concurrent hashes, and argon2's defaults ask for 19 MiB each on a blocking
/// pool 512 threads wide. Eight permits is 152 MiB and still far more parallelism than one owner
/// signing in needs. A caller that finds them all taken is answered, never queued: waiting on the
/// semaphore would move the exhaustion into an unbounded list of waiters and lose nothing.
const PASSWORD_WORK_PERMITS: usize = 8;

/// Process-wide, so every unit test in this binary shares the eight permits. A test that drains
/// them has to hold and drop them inside its own body; two such tests would race each other.
static PASSWORD_WORK: Semaphore = Semaphore::const_new(PASSWORD_WORK_PERMITS);

/// A permit to run one password check, or `None` when every permit is out.
///
/// Hold it across the whole `spawn_blocking`, not just the call that starts it, or the bound counts
/// tasks that have already finished.
pub fn password_slot() -> Option<SemaphorePermit<'static>> {
    PASSWORD_WORK.try_acquire().ok()
}

pub struct LoginLimiter {
    per_minute: u32,
    state: Mutex<State>,
}

struct State {
    by_key: HashMap<String, Vec<Instant>>,
    by_peer: HashMap<String, Vec<Instant>>,
}

/// One attempt already charged to both windows.
///
/// Dropping it keeps the charge, which is what a failed login wants. `release` gives it back.
#[must_use = "dropping this keeps the attempt on the budget; release it when the login succeeds"]
pub struct Reservation<'a> {
    limiter: &'a LoginLimiter,
    key: String,
    peer: String,
    at: Instant,
}

impl Reservation<'_> {
    /// Hand the slot back. Successes are not counted: the limit exists to slow guessing, and
    /// counting the owner's successful logins would throttle the owner for using their own server.
    pub fn release(self) {
        let mut state = self.limiter.lock();
        let State { by_key, by_peer } = &mut *state;
        remove_one(by_key.get_mut(&self.key), self.at);
        remove_one(by_peer.get_mut(&self.peer), self.at);
    }
}

impl LoginLimiter {
    pub fn new(per_minute: u32) -> Self {
        Self {
            // Zero would lock the owner out of their own server on the first typo. A configured zero
            // is read as one attempt per minute rather than none.
            per_minute: per_minute.max(1),
            state: Mutex::new(State { by_key: HashMap::new(), by_peer: HashMap::new() }),
        }
    }

    /// Take a slot for one password check, or refuse.
    ///
    /// The per-key window is read first on purpose. A key that has already spent its own budget is
    /// refused on its own account and never charges the peer window, so one caller cannot fill a
    /// budget shared with anyone else.
    pub fn reserve(&self, key: &str, peer: &str, now: Instant) -> Option<Reservation<'_>> {
        let mut state = self.lock();
        let State { by_key, by_peer } = &mut *state;

        if by_key.len() >= MAX_KEYS && !by_key.contains_key(key) {
            by_key.clear();
        }
        if by_peer.len() >= MAX_KEYS && !by_peer.contains_key(peer) {
            by_peer.clear();
        }

        let key_window = by_key.entry(key.to_string()).or_default();
        prune(key_window, now);
        if (key_window.len() as u32) >= self.per_minute {
            return None;
        }

        let peer_window = by_peer.entry(peer.to_string()).or_default();
        prune(peer_window, now);
        if (peer_window.len() as u32) >= self.per_minute.saturating_mul(PEER_MULTIPLIER) {
            return None;
        }
        peer_window.push(now);

        by_key.entry(key.to_string()).or_default().push(now);

        Some(Reservation { limiter: self, key: key.to_string(), peer: peer.to_string(), at: now })
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

/// Take back exactly one charge, not every charge sharing that instant. Two callers can reserve at
/// the same reading of the clock, and one of them succeeding must not refund the other.
fn remove_one(window: Option<&mut Vec<Instant>>, at: Instant) {
    if let Some(window) = window {
        if let Some(i) = window.iter().position(|other| *other == at) {
            window.remove(i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_attempt_from_an_unseen_address_is_allowed() {
        let limiter = LoginLimiter::new(3);
        assert!(limiter.reserve("1.2.3.4", "1.2.3.4", Instant::now()).is_some());
    }

    #[test]
    fn attempts_are_refused_once_the_window_is_full() {
        let limiter = LoginLimiter::new(3);
        let now = Instant::now();
        for _ in 0..3 {
            assert!(limiter.reserve("1.2.3.4", "1.2.3.4", now).is_some());
        }
        assert!(limiter.reserve("1.2.3.4", "1.2.3.4", now).is_none());
    }

    /// The failure this whole shape exists for. Reading the window and recording the attempt used
    /// to sit either side of Argon2id and a 750 ms delay, so a burst all read an empty budget.
    #[test]
    fn a_slot_is_spent_the_moment_it_is_taken_and_not_when_the_password_check_returns() {
        let limiter = LoginLimiter::new(2);
        let now = Instant::now();
        let first = limiter.reserve("1.2.3.4", "1.2.3.4", now).expect("first attempt");
        let second = limiter.reserve("1.2.3.4", "1.2.3.4", now).expect("second attempt");
        assert!(
            limiter.reserve("1.2.3.4", "1.2.3.4", now).is_none(),
            "two checks are still running, so the budget is already gone"
        );
        drop(first);
        drop(second);
        assert!(
            limiter.reserve("1.2.3.4", "1.2.3.4", now).is_none(),
            "dropping a guard means the attempt failed, so it stays on the budget"
        );
    }

    #[test]
    fn a_full_window_ages_out_after_a_minute() {
        let limiter = LoginLimiter::new(2);
        let now = Instant::now();
        for _ in 0..2 {
            let _spent = limiter.reserve("1.2.3.4", "1.2.3.4", now).expect("inside the budget");
        }
        assert!(limiter.reserve("1.2.3.4", "1.2.3.4", now).is_none());
        assert!(limiter.reserve("1.2.3.4", "1.2.3.4", now + Duration::from_secs(61)).is_some());
    }

    #[test]
    fn one_throttled_address_does_not_throttle_another() {
        let limiter = LoginLimiter::new(1);
        let now = Instant::now();
        let _spent = limiter.reserve("1.2.3.4", "1.2.3.4", now).expect("first attempt");
        assert!(limiter.reserve("1.2.3.4", "1.2.3.4", now).is_none());
        assert!(limiter.reserve("5.6.7.8", "5.6.7.8", now).is_some());
    }

    #[test]
    fn rotating_the_forwarded_key_still_runs_into_the_peer_window() {
        // A forwarded address is attacker-controlled, so a per-key window alone can be bypassed by
        // sending a different one each time. The socket the packets arrive on cannot be.
        let limiter = LoginLimiter::new(1);
        let now = Instant::now();
        for i in 0..PEER_MULTIPLIER {
            let key = format!("fwd:10.0.0.{i}");
            assert!(
                limiter.reserve(&key, "peer:198.51.100.7", now).is_some(),
                "attempt {i} should still be inside the budget"
            );
        }
        assert!(
            limiter.reserve("fwd:10.0.0.99", "peer:198.51.100.7", now).is_none(),
            "a fresh key must not reset the peer window"
        );
    }

    /// The lockout that made a server-wide counter the wrong shape: an unauthenticated flood used
    /// to spend one budget everybody shared, and the owner's correct password got a 429 for as long
    /// as it ran.
    #[test]
    fn a_flood_from_one_peer_leaves_another_peer_able_to_sign_in() {
        let limiter = LoginLimiter::new(1);
        let now = Instant::now();
        for i in 0..(PEER_MULTIPLIER * 4) {
            let key = format!("fwd:10.0.0.{i}");
            let _ = limiter.reserve(&key, "peer:198.51.100.7", now);
        }
        assert!(
            limiter.reserve("fwd:203.0.113.9", "peer:203.0.113.9", now).is_some(),
            "the owner arrives on a different socket and owes nothing"
        );
    }

    #[test]
    fn a_successful_login_does_not_count_against_the_owner() {
        let limiter = LoginLimiter::new(1);
        let now = Instant::now();
        let slot = limiter.reserve("1.2.3.4", "1.2.3.4", now).expect("first attempt");
        slot.release();
        assert!(
            limiter.reserve("1.2.3.4", "1.2.3.4", now).is_some(),
            "the attempt was given back, so nothing was spent"
        );
    }

    #[test]
    fn releasing_one_attempt_leaves_a_concurrent_attempt_charged() {
        let limiter = LoginLimiter::new(2);
        let now = Instant::now();
        let owner = limiter.reserve("1.2.3.4", "peer:1.2.3.4", now).expect("the owner");
        let _guesser = limiter.reserve("1.2.3.4", "peer:1.2.3.4", now).expect("someone guessing");
        owner.release();
        assert!(
            limiter.reserve("1.2.3.4", "peer:1.2.3.4", now).is_some(),
            "exactly one slot came back"
        );
        assert!(
            limiter.reserve("1.2.3.4", "peer:1.2.3.4", now).is_none(),
            "the failed attempt stayed charged"
        );
    }

    #[test]
    fn a_configured_zero_does_not_lock_the_owner_out_entirely() {
        let limiter = LoginLimiter::new(0);
        assert!(limiter.reserve("1.2.3.4", "1.2.3.4", Instant::now()).is_some());
    }

    #[test]
    fn tracked_keys_stay_bounded() {
        let limiter = LoginLimiter::new(1);
        let now = Instant::now();
        for i in 0..(MAX_KEYS + 10) {
            let _ = limiter.reserve(&format!("key-{i}"), &format!("peer-{i}"), now);
        }
        let state = limiter.lock();
        assert!(state.by_key.len() <= MAX_KEYS);
        assert!(state.by_peer.len() <= MAX_KEYS);
    }

    #[test]
    fn the_password_pool_hands_out_a_bounded_number_of_slots_and_takes_them_back() {
        let held: Vec<_> = (0..PASSWORD_WORK_PERMITS)
            .map(|i| password_slot().unwrap_or_else(|| panic!("permit {i} should be free")))
            .collect();
        assert!(
            password_slot().is_none(),
            "an unbounded pool is what put gigabytes of Argon2 memory on the blocking threads"
        );
        drop(held);
        assert!(password_slot().is_some(), "a finished check gives its permit back");
    }
}
