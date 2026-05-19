use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Returns the current date as "YYYY-MM-DD" string.
/// Falls back to "unknown" if the system clock is unreliable.
pub fn current_date_string() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();

    // Days since Unix epoch (1970-01-01)
    let days = secs / 86400;

    // Number of leap days from 1970 to year-1
    fn leap_days_since_1970(year: u64) -> u64 {
        // Leap years: divisible by 4, not by 100, unless by 400
        let y = year - 1;
        y / 4 - y / 100 + y / 400 - 469 // offset for 1970
    }

    // Approximate year, then refine
    let mut year = 1970 + days / 365;
    loop {
        let days_in_years = (year - 1970) * 365 + leap_days_since_1970(year);
        let days_in_next = (year + 1 - 1970) * 365 + leap_days_since_1970(year + 1);
        if days < days_in_next {
            let day_of_year = days - days_in_years;
            let is_leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
            let month_days: [u64; 12] = [
                31,
                if is_leap { 29 } else { 28 },
                31, 30, 31, 30, 31, 31, 30, 31, 30, 31,
            ];
            let mut remaining = day_of_year;
            for (i, &md) in month_days.iter().enumerate() {
                if remaining < md {
                    let month = i + 1;
                    let day = remaining + 1; // day_of_year is 0-based
                    return format!("{year:04}-{month:02}-{day:02}");
                }
                remaining -= md;
            }
            // Should not reach here
            return format!("{year:04}-12-31");
        }
        year += 1;
        if year > 2100 {
            return "unknown".to_owned();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_date_string_format() {
        let date = current_date_string();
        assert_eq!(date.len(), 10, "date should be YYYY-MM-DD");
        assert_eq!(&date[4..5], "-");
        assert_eq!(&date[7..8], "-");
    }
}
