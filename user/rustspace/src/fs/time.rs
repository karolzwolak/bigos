const SECONDS_PER_MINUTE: u32 = 60;
const SECONDS_PER_HOUR: u32 = 3600;
const SECONDS_PER_DAY: u32 = 86_400;
const DAYS_PER_YEAR: u32 = 365;
const DAYS_PER_FOUR_YEARS: u32 = 1461;
const DAYS_PER_CENTURY: u32 = 36_524;
const DAYS_PER_ERA: u32 = 146_097;
const YEARS_PER_ERA: u32 = 400;
const UNIX_EPOCH_SHIFT_DAYS: u32 = 719_468;
const MONTHS_BEFORE_JANUARY: u32 = 10;
const MARCH: u32 = 3;
const FEBRUARY: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub year: u32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

impl DateTime {
    pub const TEXT_LEN: usize = 19;

    /// Howard Hinnant's algorithms
    pub const fn from_unix_seconds(unix_seconds: u32) -> Self {
        let days = unix_seconds / SECONDS_PER_DAY + UNIX_EPOCH_SHIFT_DAYS;
        let second_of_day = unix_seconds % SECONDS_PER_DAY;

        let era = days / DAYS_PER_ERA;
        let day_of_era = days - era * DAYS_PER_ERA;
        let year_of_era = (day_of_era - day_of_era / (DAYS_PER_FOUR_YEARS - 1)
            + day_of_era / DAYS_PER_CENTURY
            - day_of_era / (DAYS_PER_ERA - 1))
            / DAYS_PER_YEAR;
        let day_of_year =
            day_of_era - (DAYS_PER_YEAR * year_of_era + year_of_era / 4 - year_of_era / 100);
        let shifted_month = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
        let month = if shifted_month < MONTHS_BEFORE_JANUARY {
            shifted_month + MARCH
        } else {
            shifted_month - (MONTHS_BEFORE_JANUARY - 1)
        };
        let year = year_of_era + era * YEARS_PER_ERA + (month <= FEBRUARY) as u32;

        Self {
            year,
            month,
            day,
            hour: second_of_day / SECONDS_PER_HOUR,
            minute: second_of_day % SECONDS_PER_HOUR / SECONDS_PER_MINUTE,
            second: second_of_day % SECONDS_PER_MINUTE,
        }
    }
}

impl core::fmt::Display for DateTime {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}
