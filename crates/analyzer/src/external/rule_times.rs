//! How long each host rule's own calls took in one analysis round, which the round logs at the DEBUG level, so a
//! project finds a slow rule without a profiler.

use std::collections::HashMap;
use std::time::Duration;

use mago_bytes::BytesDisplay;
use mago_extension::ValueReader;

use super::ExternalAnalyzerError;

const MAXIMUM_RULE_CALLS: usize = 10_000_000;
const SLOWEST_FILES: usize = 5;

/// How long one rule's calls took on one file, or on the project when they name none, as a worker answered it.
#[derive(Debug)]
pub(super) struct RuleCalls {
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

/// How long each rule took in one round, by its class and method, as `App\Rules\NoRawQuery::analyze`.
#[derive(Debug, Default)]
pub(super) struct RuleTimes(HashMap<String, RuleTime>);

impl RuleTimes {
    /// Reads the rule times a lifecycle response ends with: each rule, the file its calls ran on or none, and how many
    /// nanoseconds they took.
    pub(super) fn decode(reader: &mut ValueReader<'_>) -> Result<Vec<RuleCalls>, ExternalAnalyzerError> {
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

    pub(super) fn record(&mut self, calls: Vec<RuleCalls>) {
        for RuleCalls { rule, file, took } in calls {
            let time = self.0.entry(rule).or_default();
            time.took += took;
            if let Some(file) = file {
                *time.files.entry(file).or_default() += took;
            }
        }
    }

    /// Logs each rule's time, slowest first, then the slowest files of the slowest rule.
    pub(super) fn log(&self) {
        let mut rules: Vec<(&String, &RuleTime)> = self.0.iter().collect();
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
            tracing::debug!("Host rule {rule} took {:.2} s on {}.", took.as_secs_f64(), BytesDisplay(file));
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
