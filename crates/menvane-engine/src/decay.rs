pub struct DecayEngine;

impl DecayEngine {
    pub fn session_retention(
        age_days: f64,
        meaningful_access_count: u64,
        days_since_access: f64,
    ) -> f64 {
        let time_score = (-std::f64::consts::LN_2 * age_days / 45.0).exp();
        let access_score = 0.25
            * (1.0 + meaningful_access_count as f64).ln()
            * (-std::f64::consts::LN_2 * days_since_access / 60.0).exp();
        time_score + access_score
    }

    pub fn freshness(memory_type: &str, age_days: f64) -> f64 {
        match memory_type {
            "fact" | "gotcha" => 0.50_f64.max((-std::f64::consts::LN_2 * age_days / 180.0).exp()),
            "procedure" => 0.65_f64.max((-std::f64::consts::LN_2 * age_days / 365.0).exp()),
            "session" => (-std::f64::consts::LN_2 * age_days / 45.0).exp(),
            _ => 1.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decay_is_type_specific_and_retains_procedures() {
        assert!(DecayEngine::session_retention(180.0, 0, 180.0) < 0.15);
        assert_eq!(DecayEngine::freshness("decision", 10_000.0), 1.0);
        assert_eq!(DecayEngine::freshness("procedure", 10_000.0), 0.65);
        assert_eq!(DecayEngine::freshness("fact", 10_000.0), 0.50);
    }
}
