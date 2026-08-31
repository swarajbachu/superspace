//! Central motion catalog. All values are pure and testable.

use std::time::Duration;

use gpui::Animation;

/// A CSS-style cubic Bézier timing curve.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CubicBezier {
    /// First control-point x.
    pub x1: f32,
    /// First control-point y.
    pub y1: f32,
    /// Second control-point x.
    pub x2: f32,
    /// Second control-point y.
    pub y2: f32,
}

impl CubicBezier {
    /// Construct a curve with fixed `(0,0)` and `(1,1)` endpoints.
    pub const fn new(x1: f32, y1: f32, x2: f32, y2: f32) -> Self {
        Self { x1, y1, x2, y2 }
    }

    fn coefficients(first: f32, second: f32) -> (f32, f32, f32) {
        let c = 3.0 * first;
        let b = 3.0 * (second - first) - c;
        (1.0 - c - b, b, c)
    }

    fn sample(first: f32, second: f32, t: f32) -> f32 {
        let (a, b, c) = Self::coefficients(first, second);
        ((a * t + b) * t + c) * t
    }

    fn derivative(first: f32, second: f32, t: f32) -> f32 {
        let (a, b, c) = Self::coefficients(first, second);
        (3.0 * a * t + 2.0 * b) * t + c
    }

    /// Evaluate an input progress in the closed range `0..=1`.
    #[must_use]
    pub fn evaluate(self, progress: f32) -> f32 {
        let progress = progress.clamp(0.0, 1.0);
        if progress == 0.0 || progress == 1.0 {
            return progress;
        }
        let mut t = progress;
        for _ in 0..8 {
            let error = Self::sample(self.x1, self.x2, t) - progress;
            let derivative = Self::derivative(self.x1, self.x2, t);
            if derivative.abs() < 0.000_001 {
                break;
            }
            t = (t - error / derivative).clamp(0.0, 1.0);
        }
        Self::sample(self.y1, self.y2, t).clamp(0.0, 1.0)
    }
}

/// One named transition.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Motion {
    /// Wall-clock duration.
    pub duration: Duration,
    /// Easing curve.
    pub curve: CubicBezier,
}

impl Motion {
    /// Build a GPUI one-shot animation.
    #[must_use]
    pub fn animation(self) -> Animation {
        Animation::new(self.duration).with_easing(move |progress| self.curve.evaluate(progress))
    }
}

/// Palette and content entrance.
pub const ENTER: Motion = Motion {
    duration: Duration::from_millis(220),
    curve: CubicBezier::new(0.16, 1.0, 0.3, 1.0),
};
/// Quick selection and hover transition.
pub const QUICK: Motion = Motion {
    duration: Duration::from_millis(140),
    curve: CubicBezier::new(0.4, 0.0, 0.2, 1.0),
};
/// Popover/dialog exit, intentionally faster than entry.
pub const EXIT: Motion = Motion {
    duration: Duration::from_millis(100),
    curve: CubicBezier::new(0.4, 0.0, 1.0, 1.0),
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves_hold_endpoints_and_remain_bounded() {
        for motion in [ENTER, QUICK, EXIT] {
            assert_eq!(motion.curve.evaluate(0.0), 0.0);
            assert_eq!(motion.curve.evaluate(1.0), 1.0);
            for step in 0..=100 {
                assert!((0.0..=1.0).contains(&motion.curve.evaluate(step as f32 / 100.0)));
            }
        }
    }
}
