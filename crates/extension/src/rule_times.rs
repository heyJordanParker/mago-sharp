//! How long each host rule's own calls took in one run of mago's analysis or linter, which the run logs at the DEBUG
//! level when it ends, so a project finds a slow rule without a profiler.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::time::Duration;

use crate::PayloadError;
use crate::ValueReader;

const MAXIMUM_RULE_CALLS: usize = 10_000_000;
const SLOWEST_FILES: usize = 5;

/// How long one rule's calls took on one file, or on none, as a worker answered it.
#[derive(Debug)]
pub struct RuleCalls {
    rule: String,
    file: Option<Vec<u8>>,
    took: Duration,
}

/// How long one rule took in all, and on each file it ran on.
#[derive(Debug, Default)]
struct RuleTime {
    took: Duration,
    files: HashMap<Vec<u8>, Duration>,
}

/// How long each host rule took in one run, by its class and method, as `App\Rules\NoRawQuery::analyze`.
///
/// It holds them only when mago logs at the DEBUG level, so only then does mago ask each worker to time its rules, and
/// it logs them when it is dropped, as the run ends.
#[derive(Debug)]
pub struct RuleTimes(Option<Mutex<HashMap<String, RuleTime>>>);

impl RuleTimes {
    #[must_use]
    pub fn new() -> Self {
        Self(tracing::enabled!(tracing::Level::DEBUG).then(Mutex::default))
    }

    /// Whether mago asks each worker how long its rules took.
    #[must_use]
    pub fn asked(&self) -> bool {
        self.0.is_some()
    }

    /// Reads the rule times a worker's response carries: a count, then each rule, the file its calls ran on or none,
    /// and how many nanoseconds they took.
    ///
    /// # Errors
    ///
    /// Returns an error when the response holds no rule times where they belong.
    pub fn decode(reader: &mut ValueReader<'_>) -> Result<Vec<RuleCalls>, PayloadError> {
        let count = reader.read_count("host rule times", MAXIMUM_RULE_CALLS)?;
        let mut calls = Vec::with_capacity(count);
        for _ in 0..count {
            let rule = reader.read_string("host rule")?;
            let file = reader.read_optional_bytes("host rule file")?.map(<[u8]>::to_vec);
            let took = Duration::from_nanos(reader.read_u64("host rule nanoseconds")?);
            calls.push(RuleCalls { rule, file, took });
        }

        Ok(calls)
    }

    /// Adds `calls`, those that name no file on `file`, the file mago analyzed or linted when it asked.
    pub fn record(&self, calls: Vec<RuleCalls>, file: Option<&[u8]>) {
        let Some(rules) = &self.0 else {
            return;
        };
        let mut rules = rules.lock().unwrap_or_else(PoisonError::into_inner);
        for RuleCalls { rule, file: named, took } in calls {
            let time = rules.entry(rule).or_default();
            time.took += took;
            if let Some(file) = named.as_deref().or(file) {
                *time.files.entry(file.to_vec()).or_default() += took;
            }
        }
    }
}

impl Default for RuleTimes {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for RuleTimes {
    /// Logs each rule's time, slowest first, then the slowest files of the slowest rule.
    fn drop(&mut self) {
        let Some(rules) = &mut self.0 else {
            return;
        };
        let rules = rules.get_mut().unwrap_or_else(PoisonError::into_inner);
        let mut rules: Vec<(&String, &RuleTime)> = rules.iter().collect();
        rules.sort_by(|(a, a_time), (b, b_time)| b_time.took.cmp(&a_time.took).then_with(|| a.cmp(b)));
        for (rule, time) in &rules {
            let took = time.took.as_secs_f64();
            match time.files.len() {
                0 => tracing::debug!("Host rule {rule} took {took:.2} s."),
                1 => tracing::debug!("Host rule {rule} took {took:.2} s over 1 file."),
                files => tracing::debug!("Host rule {rule} took {took:.2} s over {} files.", grouped(files)),
            }
        }

        let Some((rule, time)) = rules.first() else {
            return;
        };
        let mut files: Vec<(&Vec<u8>, &Duration)> = time.files.iter().collect();
        files.sort_by(|(a, a_took), (b, b_took)| b_took.cmp(a_took).then_with(|| a.cmp(b)));
        for (file, took) in files.into_iter().take(SLOWEST_FILES) {
            tracing::debug!("Host rule {rule} took {:.2} s on {}.", took.as_secs_f64(), String::from_utf8_lossy(file));
        }
    }
}

/// `count` with a comma between each group of three digits, as `1,204`.
fn grouped(count: usize) -> String {
    let digits = count.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (place, digit) in digits.chars().enumerate() {
        if place != 0 && (digits.len() - place).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }

    grouped
}
