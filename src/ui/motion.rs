// SPDX-License-Identifier: MIT

use std::cell::Cell;

thread_local! {
    static REDUCE_MOTION: Cell<bool> = const { Cell::new(false) };
}

pub(super) fn set_reduce_motion(reduced: bool) {
    REDUCE_MOTION.set(reduced);
}

pub(super) fn animations_enabled() -> bool {
    !REDUCE_MOTION.get()
        && gtk::Settings::default()
            .map(|settings| settings.is_gtk_enable_animations())
            .unwrap_or(true)
}

/// Starts fast and settles slowly; for things that appear.
pub(super) fn emphasized_deceleration(progress: f64) -> f64 {
    cubic_bezier(progress, 0.16, 1.0, 0.3, 1.0)
}

/// Eases in and out, for a surface that travels between two places.
pub(super) fn emphasized(progress: f64) -> f64 {
    cubic_bezier(progress, 0.2, 0.0, 0.0, 1.0)
}

fn cubic_bezier(progress: f64, first_x: f64, first_y: f64, second_x: f64, second_y: f64) -> f64 {
    let progress = progress.clamp(0.0, 1.0);
    let mut lower = 0.0;
    let mut upper = 1.0;
    for _ in 0..16 {
        let time = (lower + upper) / 2.0;
        if cubic_coordinate(time, first_x, second_x) < progress {
            lower = time;
        } else {
            upper = time;
        }
    }
    cubic_coordinate((lower + upper) / 2.0, first_y, second_y)
}

fn cubic_coordinate(time: f64, first: f64, second: f64) -> f64 {
    let inverse = 1.0 - time;
    3.0 * inverse * inverse * time * first
        + 3.0 * inverse * time * time * second
        + time * time * time
}

#[cfg(test)]
mod tests;
