use std::prelude::v1::*;

use crate::nanos::Nanos;
use crate::state::StateStore;
use crate::{clock, NegativeMultiDecision, Quota};
use std::num::NonZeroU32;
use std::time::Duration;
use std::{cmp, fmt};

#[cfg(feature = "std")]
use crate::Jitter;

/// A negative rate-limiting outcome.
///
/// `NotUntil`'s methods indicate when a caller can expect the next positive
/// rate-limiting result.
#[derive(Debug, PartialEq)]
pub struct NotUntil<'a, P: clock::Reference> {
    limiter: &'a Gcra,
    tat: Nanos,
    start: P,
}

impl<'a, P: clock::Reference> NotUntil<'a, P> {
    /// Returns the earliest time at which a decision could be
    /// conforming (excluding conforming decisions made by the Decider
    /// that are made in the meantime).
    pub fn earliest_possible(&self) -> P {
        let tat: Nanos = self.tat;
        self.start + tat
    }

    /// Returns the minimum amount of time from the time that the
    /// decision was made that must pass before a
    /// decision can be conforming.
    ///
    /// If the time of the next expected positive result is in the past,
    /// `wait_time_from` returns a zero `Duration`.
    pub fn wait_time_from(&self, from: P) -> Duration {
        let earliest = self.earliest_possible();
        earliest.duration_since(earliest.min(from)).into()
    }

    #[cfg(feature = "std")] // not used unless we use Instant-compatible clocks.
    pub(crate) fn earliest_possible_with_offset(&self, jitter: Jitter) -> P {
        let tat = jitter + self.tat;
        self.start + tat
    }

    #[cfg(feature = "std")] // not used unless we use Instant-compatible clocks.
    pub(crate) fn wait_time_with_offset(&self, from: P, jitter: Jitter) -> Duration {
        let earliest = self.earliest_possible_with_offset(jitter);
        earliest.duration_since(earliest.min(from)).into()
    }
}

impl<'a, P: clock::Reference> fmt::Display for NotUntil<'a, P> {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(f, "rate-limited until {:?}", self.start + self.tat)
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct Gcra {
    // The "weight" of a single packet in units of time.
    t: Nanos,

    // The "capacity" of the bucket.
    tau: Nanos,
}

impl Gcra {
    pub(crate) fn new(quota: Quota) -> Self {
        let tau: Nanos = (quota.replenish_1_per * quota.max_burst.get()).into();
        let t: Nanos = quota.replenish_1_per.into();
        Gcra { tau, t }
    }

    /// Computes and returns a new ratelimiter state if none exists yet.
    fn starting_state(&self, t0: Nanos) -> Nanos {
        t0 + self.t
    }

    /// Tests a single cell against the rate limiter state and updates it at the given key.
    pub(crate) fn test_and_update<K, P: clock::Reference>(
        &self,
        start: P,
        key: &K,
        state: &impl StateStore<Key = K>,
        t0: P,
    ) -> Result<(), NotUntil<P>> {
        let t0 = t0.duration_since(start);
        let tau = self.tau;
        let t = self.t;
        state.measure_and_replace(key, |tat| {
            let tat = tat.unwrap_or_else(|| self.starting_state(t0));
            let earliest_time = tat.saturating_sub(tau);
            if t0 < earliest_time {
                Err(NotUntil {
                    limiter: self,
                    tat: earliest_time,
                    start,
                })
            } else {
                Ok(((), cmp::max(tat, t0) + t))
            }
        })
    }

    /// Tests whether all `n` cells could be accommodated and updates the rate limiter state, if so.
    pub(crate) fn test_n_all_and_update<K, P: clock::Reference>(
        &self,
        start: P,
        key: &K,
        n: NonZeroU32,
        state: &impl StateStore<Key = K>,
        t0: P,
    ) -> Result<(), NegativeMultiDecision<NotUntil<P>>> {
        let t0 = t0.duration_since(start);
        let tau = self.tau;
        let t = self.t;
        let additional_weight = t * (n.get() - 1) as u64;

        // check that we can allow enough cells through. Note that `additional_weight` is the
        // value of the cells *in addition* to the first cell - so add that first cell back.
        if additional_weight + t > tau {
            return Err(NegativeMultiDecision::InsufficientCapacity(
                (tau.as_u64() / t.as_u64()) as u32,
            ));
        }
        state.measure_and_replace(key, |tat| {
            let tat = tat.unwrap_or_else(|| self.starting_state(t0));
            let earliest_time = (tat + additional_weight).saturating_sub(tau);
            if t0 < earliest_time {
                Err(NegativeMultiDecision::BatchNonConforming(
                    n.get(),
                    NotUntil {
                        limiter: self,
                        tat: earliest_time,
                        start,
                    },
                ))
            } else {
                Ok(((), cmp::max(tat, t0) + t + additional_weight))
            }
        })
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{Quota, RateLimiter};
    use clock::FakeRelativeClock;
    use nonzero_ext::nonzero;

    /// Exercise derives and convenience impls on Gcra to make coverage happy
    #[test]
    fn gcra_derives() {
        let g = Gcra::new(Quota::per_second(nonzero!(1u32)));
        let g2 = Gcra::new(Quota::per_second(nonzero!(2u32)));
        assert_eq!(g, g);
        assert_ne!(g, g2);
        assert!(format!("{:?}", g).len() > 0);
    }

    /// Exercise derives and convenience impls on NotUntil to make coverage happy
    #[test]
    fn notuntil_impls() {
        let clock = FakeRelativeClock::default();
        let lb = RateLimiter::direct_with_clock(Quota::per_second(nonzero!(1u32)), &clock);
        assert!(lb.check().is_ok());
        if let Err(nu) = lb.check() {
            assert_eq!(nu, nu);
            assert!(format!("{:?}", nu).len() > 0);
            assert_eq!(format!("{}", nu), "rate-limited until Nanos(1s)");
        }
    }
}
