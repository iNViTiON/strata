// SPDX-License-Identifier: MIT

use super::*;

#[test]
fn emphasized_deceleration_has_expected_endpoints() {
    assert!(emphasized_deceleration(0.0).abs() < 0.0001);
    assert!((emphasized_deceleration(1.0) - 1.0).abs() < 0.0001);
}

#[test]
fn emphasized_deceleration_is_monotonic_and_front_loaded() {
    let samples: Vec<_> = (0..=20)
        .map(|sample| emphasized_deceleration(f64::from(sample) / 20.0))
        .collect();

    assert!(samples.windows(2).all(|pair| pair[0] <= pair[1]));
    assert!(emphasized_deceleration(0.5) > 0.8);
}

#[test]
fn emphasized_starts_gently_and_ends_at_rest() {
    assert!(emphasized(0.0).abs() < 0.0001);
    assert!((emphasized(1.0) - 1.0).abs() < 0.0001);
    let samples: Vec<_> = (0..=20)
        .map(|sample| emphasized(f64::from(sample) / 20.0))
        .collect();

    assert!(samples.windows(2).all(|pair| pair[0] <= pair[1]));
    assert!(emphasized(0.1) < emphasized_deceleration(0.1));
}
