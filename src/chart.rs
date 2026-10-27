//! Speed-chart geometry shared by the dashboard and the task drawer.

use std::collections::VecDeque;

/// Builds the `Path` commands for the speed chart in a 0..1 viewbox.
/// Returns `(line, area)`.
pub fn chart_paths(history: &VecDeque<f64>) -> (String, String) {
    if history.is_empty() {
        return (String::new(), String::new());
    }
    let peak = history.iter().copied().fold(1.0_f64, f64::max).max(1.0);
    let n = history.len();
    let mut line = String::with_capacity(n * 18);
    let mut area = String::with_capacity(n * 18 + 24);
    area.push_str("M 0 1");

    for (i, value) in history.iter().enumerate() {
        let x = i as f64 / (n.saturating_sub(1)).max(1) as f64;
        let y = 0.94 - (value / peak).clamp(0.0, 1.0) * 0.88;
        if i == 0 {
            line.push_str(&format!("M {x:.4} {y:.4}"));
        } else {
            line.push_str(&format!(" L {x:.4} {y:.4}"));
        }
        area.push_str(&format!(" L {x:.4} {y:.4}"));
    }
    area.push_str(" L 1 1 Z");
    (line, area)
}

/// Peak of the history, used for the chart caption.
pub fn chart_peak(history: &VecDeque<f64>) -> f64 {
    history.iter().copied().fold(0.0, f64::max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_history_has_no_geometry() {
        let history = VecDeque::new();
        assert_eq!(chart_paths(&history), (String::new(), String::new()));
        assert_eq!(chart_peak(&history), 0.0);
    }

    #[test]
    fn paths_stay_inside_the_viewbox() {
        let history = VecDeque::from(vec![0.0, 512.0, 1024.0]);
        let (line, area) = chart_paths(&history);
        // First sample 0 -> y = 0.94; last sample is the peak -> y = 0.06.
        assert!(line.starts_with("M 0.0000 0.9400"), "{line}");
        assert!(line.ends_with("0.0600"), "{line}");
        assert!(area.starts_with("M 0 1"));
        assert!(area.ends_with("L 1 1 Z"));
        assert_eq!(chart_peak(&history), 1024.0);
    }
}
