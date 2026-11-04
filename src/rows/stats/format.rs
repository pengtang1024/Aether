//! Pure formatting helpers for the dashboard cards.
//!
//! Kept apart from the Slint setters so they stay unit-testable: no window,
//! no state, no i18n lookups beyond the explicit `version_text` call.

/// Integer percent for compact cards. A nonzero slice that rounds to zero
/// reads "<1%" instead of a misleading "0%".
pub(super) fn percent_text(part: u64, total: u64) -> String {
    if total == 0 {
        return "0%".to_string();
    }
    let rounded = (part as f64 / total as f64 * 100.0).round() as u64;
    if part > 0 && rounded == 0 {
        "<1%".to_string()
    } else {
        format!("{rounded}%")
    }
}

/// Share of `part` in `total` as 0..=1, or 0 when there is nothing to divide.
pub(super) fn share(part: u64, total: u64) -> f32 {
    if total == 0 {
        0.0
    } else {
        (part as f64 / total as f64).clamp(0.0, 1.0) as f32
    }
}

/// Same as `share`, for the floating-point history used by the weekly bars.
pub(super) fn ratio(value: f64, max: f64) -> f32 {
    if max <= 0.0 {
        0.0
    } else {
        (value / max).clamp(0.0, 1.0) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::{percent_text, ratio, share};

    #[test]
    fn empty_total_reads_zero() {
        assert_eq!(percent_text(0, 0), "0%");
        assert_eq!(share(0, 0), 0.0);
    }

    #[test]
    fn nonzero_slice_never_reads_zero() {
        assert_eq!(percent_text(18, 7_700), "<1%");
    }

    #[test]
    fn rounds_half_away_from_zero() {
        assert_eq!(percent_text(1, 200), "1%");
        assert_eq!(percent_text(1, 2), "50%");
    }

    #[test]
    fn share_and_ratio_clamp() {
        assert_eq!(share(5, 2), 1.0);
        assert_eq!(share(1, 4), 0.25);
        assert_eq!(ratio(3.0, 0.0), 0.0);
        assert_eq!(ratio(6.0, 4.0), 1.0);
    }
}
