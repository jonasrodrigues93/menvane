use chrono::{DateTime, Utc};

pub const DEFAULT_MEMORY_LIFETIME_DAYS: f64 = 90.0;
const FORGET_THRESHOLD: f64 = 0.15;
const REINFORCEMENT_WEIGHT: f64 = 0.25;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MemoryDecay {
    pub score: f64,
    pub days_remaining: f64,
}

pub fn memory_decay(
    created_at: DateTime<Utc>,
    reinforcement_count: u64,
    last_reinforced_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    lifetime_days: f64,
) -> MemoryDecay {
    let age_days = days_between(created_at, now);
    let since_reinforcement = last_reinforced_at
        .map(|value| days_between(value, now))
        .unwrap_or(age_days);
    let score = normalized_score(
        age_days,
        reinforcement_count,
        since_reinforcement,
        lifetime_days,
    );
    let days_remaining = if score == 0.0 {
        0.0
    } else {
        days_until_zero(
            age_days,
            reinforcement_count,
            since_reinforcement,
            lifetime_days,
        )
    };
    MemoryDecay {
        score,
        days_remaining,
    }
}

fn normalized_score(
    age_days: f64,
    reinforcement_count: u64,
    days_since_reinforcement: f64,
    lifetime_days: f64,
) -> f64 {
    let lifetime_days = lifetime_days.max(1.0);
    let time_score = (FORGET_THRESHOLD.ln() * age_days / lifetime_days).exp();
    let reinforcement_half_life = lifetime_days * 4.0 / 3.0;
    let reinforcement_score = REINFORCEMENT_WEIGHT
        * (1.0 + reinforcement_count as f64).ln()
        * (-std::f64::consts::LN_2 * days_since_reinforcement / reinforcement_half_life).exp();
    ((time_score + reinforcement_score - FORGET_THRESHOLD) / (1.0 - FORGET_THRESHOLD))
        .clamp(0.0, 1.0)
}

fn days_until_zero(
    age_days: f64,
    reinforcement_count: u64,
    days_since_reinforcement: f64,
    lifetime_days: f64,
) -> f64 {
    let mut low = 0.0;
    let mut high = lifetime_days.max(1.0);
    while normalized_score(
        age_days + high,
        reinforcement_count,
        days_since_reinforcement + high,
        lifetime_days,
    ) > 0.0
    {
        high *= 2.0;
        if high > lifetime_days * 100.0 {
            return high;
        }
    }
    for _ in 0..48 {
        let middle = (low + high) / 2.0;
        if normalized_score(
            age_days + middle,
            reinforcement_count,
            days_since_reinforcement + middle,
            lifetime_days,
        ) > 0.0
        {
            low = middle;
        } else {
            high = middle;
        }
    }
    high
}

fn days_between(earlier: DateTime<Utc>, later: DateTime<Utc>) -> f64 {
    (later - earlier).num_seconds().max(0) as f64 / 86_400.0
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    #[test]
    fn unreinforced_memory_reaches_zero_at_lifetime() {
        let created = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let before = memory_decay(created, 0, None, created + chrono::Duration::days(89), 90.0);
        let expired = memory_decay(created, 0, None, created + chrono::Duration::days(90), 90.0);
        assert!(before.score > 0.0);
        assert_eq!(expired.score, 0.0);
    }

    #[test]
    fn recent_reinforcement_extends_lifetime_with_logarithmic_bonus() {
        let created = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let now = created + chrono::Duration::days(90);
        let reinforced = memory_decay(created, 2, Some(now), now, 90.0);
        assert!(reinforced.score > 0.0);
        assert!(reinforced.days_remaining > 0.0);
    }
}
