pub struct DecayEngine;

#[derive(Debug, Clone, Copy, serde::Deserialize)]
pub struct DecayConfiguration {
    pub fact_gotcha_half_life_days: f64,
    pub fact_gotcha_floor: f64,
    pub procedure_half_life_days: f64,
    pub procedure_floor: f64,
    pub session_half_life_days: f64,
}

impl Default for DecayConfiguration {
    fn default() -> Self {
        Self {
            fact_gotcha_half_life_days: 180.0,
            fact_gotcha_floor: 0.50,
            procedure_half_life_days: 365.0,
            procedure_floor: 0.65,
            session_half_life_days: 45.0,
        }
    }
}

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
        Self::freshness_with(&DecayConfiguration::default(), memory_type, age_days)
    }

    pub fn freshness_with(config: &DecayConfiguration, memory_type: &str, age_days: f64) -> f64 {
        match memory_type {
            "fact" | "gotcha" => config.fact_gotcha_floor.max(
                (-std::f64::consts::LN_2 * age_days / config.fact_gotcha_half_life_days).exp(),
            ),
            "procedure" => config
                .procedure_floor
                .max((-std::f64::consts::LN_2 * age_days / config.procedure_half_life_days).exp()),
            "session" => (-std::f64::consts::LN_2 * age_days / config.session_half_life_days).exp(),
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
