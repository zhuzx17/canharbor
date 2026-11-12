// SPDX-License-Identifier: AGPL-3.0-or-later
//! Compare clocks with/without suspended time; wall-clock adjustments are irrelevant.
#[derive(Clone, Copy)]
pub(crate) struct Sample {
    lower: u128,
    upper: u128,
}
const TOLERANCE_NS: u128 = 1_000_000;
// A preemption between clock reads makes that bracket inconclusive. Retry the
// measurement rather than treating one delayed sample as a suspend event.
fn sample_bracket(mut read: impl FnMut() -> Option<(u128, u128, u128)>) -> Option<Sample> {
    for _ in 0..8 {
        let (before, active, after) = read()?;
        if after >= before && after - before > TOLERANCE_NS {
            continue;
        }
        return Sample::bracket(before, active, after);
    }
    None
}
impl Sample {
    fn bracket(before: u128, active: u128, after: u128) -> Option<Self> {
        if after < before || after - before > TOLERANCE_NS || after < active {
            return None;
        }
        Some(Self {
            lower: before.saturating_sub(active),
            upper: after - active,
        })
    }
}
pub(crate) fn valid(initial: Option<Sample>, current: Option<Sample>) -> bool {
    match (initial, current) {
        (Some(a), Some(b)) => {
            b.lower <= a.upper.saturating_add(TOLERANCE_NS)
                && a.lower <= b.upper.saturating_add(TOLERANCE_NS)
        }
        _ => false,
    }
}
pub(crate) fn sample() -> Option<Sample> {
    sample_bracket(canharbor_platform::clock::readings)
}
#[cfg(test)]
pub(crate) fn shifted_sample() -> Option<Sample> {
    sample().map(|s| Sample {
        lower: s.lower + 10_000_000,
        upper: s.upper + 10_000_000,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delayed_sample_is_retried_without_hiding_suspend_or_clock_failure() {
        let initial = Sample::bracket(1_000_000, 500_000, 1_000_100);
        for (next, expected) in [
            ((11_000_000, 10_500_000, 11_000_100), true),
            ((21_000_000, 10_500_000, 21_000_100), false),
        ] {
            let mut calls = 0;
            let current = sample_bracket(|| {
                calls += 1;
                Some(if calls == 1 {
                    (0, 0, TOLERANCE_NS + 1)
                } else {
                    next
                })
            });
            assert_eq!(calls, 2);
            assert_eq!(valid(initial, current), expected);
        }
        let mut calls = 0;
        assert!(
            sample_bracket(|| {
                calls += 1;
                Some((0, 0, TOLERANCE_NS + 1))
            })
            .is_none()
        );
        assert_eq!(calls, 8);
        assert!(!valid(initial, sample_bracket(|| None)));
        assert!(!valid(initial, sample_bracket(|| Some((10, 0, 9)))));
    }
    #[test]
    fn active_progress_is_valid_but_suspend_and_uncertain_clocks_are_not() {
        let before = Sample::bracket(1_000_000, 500_000, 1_000_100);
        assert!(valid(
            before,
            Sample::bracket(11_000_000, 10_500_000, 11_000_100)
        ));
        assert!(!valid(
            before,
            Sample::bracket(21_000_000, 10_500_000, 21_000_100)
        ));
        assert!(!valid(before, None));
        assert!(Sample::bracket(0, 0, TOLERANCE_NS + 1).is_none());
        assert!(Sample::bracket(10, 0, 9).is_none());
    }
    #[test]
    fn native_samples_are_consistent_without_suspending_the_machine() {
        assert!(valid(sample(), sample()));
    }
}
