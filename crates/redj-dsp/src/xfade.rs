//! Crossfader curves.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CrossfaderCurve {
    /// Equal power: smooth blends, no dip in the middle.
    #[default]
    ConstantPower,
    /// Linear amplitude.
    Linear,
    /// Scratch cut: both sides at full level except near the edges.
    Sharp,
}

/// Gains of the left and right side for a crossfader position in 0..1
/// (0 = full left).
pub fn crossfader_gains(pos: f32, curve: CrossfaderCurve) -> (f32, f32) {
    let x = pos.clamp(0.0, 1.0);
    match curve {
        CrossfaderCurve::ConstantPower => {
            let a = x * std::f32::consts::FRAC_PI_2;
            (a.cos(), a.sin())
        }
        CrossfaderCurve::Linear => (1.0 - x, x),
        CrossfaderCurve::Sharp => {
            let edge = 0.05;
            (((1.0 - x) / edge).min(1.0), (x / edge).min(1.0))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves() {
        let (a, b) = crossfader_gains(0.5, CrossfaderCurve::ConstantPower);
        assert!((a * a + b * b - 1.0).abs() < 1e-6);
        assert_eq!(crossfader_gains(0.0, CrossfaderCurve::Linear), (1.0, 0.0));
        assert_eq!(crossfader_gains(0.5, CrossfaderCurve::Sharp), (1.0, 1.0));
        assert_eq!(crossfader_gains(1.0, CrossfaderCurve::Sharp).0, 0.0);
    }
}
