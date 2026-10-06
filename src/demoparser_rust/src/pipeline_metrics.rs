/// Aggregate throughput in wall seconds per completed source demo, never worker time.
/// Empty/skipped-only runs have no throughput sample.
pub fn seconds_per_demo(elapsed: f64, completed: usize) -> Option<f64> {
    (completed > 0 && elapsed.is_finite() && elapsed >= 0.0).then(|| elapsed / completed as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_completed_demos_form_the_denominator() {
        assert_eq!(seconds_per_demo(12.0, 4), Some(3.0));
        assert_eq!(seconds_per_demo(12.0, 0), None);
        assert_eq!(seconds_per_demo(f64::NAN, 4), None);
        assert_eq!(seconds_per_demo(f64::INFINITY, 4), None);
    }
}
