use std::borrow::Cow;
use std::io::IsTerminal;

use clap::ColorChoice;
use diffy::PatchFormatter;

use mago_database::change::ChangeLog;
use mago_database::file::File;
use mago_linter::integration::IntegrationSet;
use mago_linter::settings::RulesSettings;
use mago_linter::settings::Settings;
use mago_orchestrator::Orchestrator;
use mago_orchestrator::OrchestratorConfiguration;
use mago_reporting::ColorChoice as ReportingColorChoice;

use crate::config::Configuration;
use crate::consts::SHARP_EXTENSION;
use crate::error::Error;

pub mod git;
pub mod logger;
pub mod progress;
pub mod version;

/// Determines whether colors should be used on stdout, by the rule [`ReportingColorChoice::should_use_colors`] holds.
#[inline]
pub fn should_use_colors(color_choice: ColorChoice) -> bool {
    let color_choice = match color_choice {
        ColorChoice::Auto => ReportingColorChoice::Auto,
        ColorChoice::Always => ReportingColorChoice::Always,
        ColorChoice::Never => ReportingColorChoice::Never,
    };

    color_choice.should_use_colors(std::io::stdout().is_terminal())
}

/// Configures global color settings based on the color choice.
///
/// This should be called early in the application to ensure consistent color behavior
/// across all crates that respect global color settings (like `colored`).
#[inline]
pub fn configure_colors(color_choice: ColorChoice) {
    let use_colors = should_use_colors(color_choice);
    colored::control::set_override(use_colors);
}

pub(crate) fn create_orchestrator<'a>(
    configuration: &'a Configuration,
    color_choice: ColorChoice,
    pedantic_linter: bool,
    use_progress_bars: bool,
    enable_diff: bool,
) -> Orchestrator<'a> {
    let glob = configuration.source.glob.to_database_settings();
    let linter_settings = if pedantic_linter {
        Settings {
            php_version: configuration.php_version,
            integrations: IntegrationSet::all(),
            rules: RulesSettings::default(),
            glob,
        }
    } else {
        Settings {
            php_version: configuration.php_version,
            integrations: IntegrationSet::from_slice(&configuration.linter.integrations),
            rules: configuration.linter.rules.clone(),
            glob,
        }
    };

    let orchestrator_config = OrchestratorConfiguration {
        php_version: configuration.php_version,
        parser_settings: configuration.parser.to_settings(),
        analyzer_settings: configuration.analyzer.to_settings(configuration.php_version, color_choice, enable_diff),
        linter_settings,
        guard_settings: configuration.guard.settings.clone(),
        formatter_settings: configuration.formatter.settings,
        disable_default_analyzer_plugins: configuration.analyzer.disable_default_plugins,
        analyzer_plugins: configuration.analyzer.plugins.clone(),
        use_progress_bars,
        use_colors: should_use_colors(color_choice),
        paths: configuration.source.paths.clone(),
        excludes: configuration.source.excludes.iter().map(|p| p.as_ref()).collect(),
        extensions: configuration.source.extensions.iter().map(|e| e.as_ref()).collect(),
        includes: configuration.source.includes.clone(),
        patches: configuration.source.patches.clone(),
        glob,
    };

    Orchestrator::new(orchestrator_config)
}

/// Leaves PHP# files out of a tool that reads only PHP, so a scan never reads them.
///
/// A PHP# file named on the command line bypasses the extension filter, so the tool refuses it with an error.
pub(crate) fn skip_sharp_files(orchestrator: &mut Orchestrator<'_>) {
    orchestrator.config.extensions.retain(|extension| *extension != SHARP_EXTENSION);
}

/// Processes the result of a modifying a single file.
///
/// This function compares the original file content with the newly modified content.
/// If there's a difference, it either prints a colorized diff to the console (if in
/// `dry_run` mode) or records an update operation in the provided [`ChangeLog`].
///
/// # Arguments
///
/// * `change_log`: The log where file updates are recorded when not in dry-run mode.
/// * `file`: The original file, used for comparison and context.
/// * `modified_contents`: The newly modified content.
/// * `dry_run`: If `true`, a diff is printed to standard output; otherwise, the
///   change is recorded in the `change_log`.
///
/// # Returns
///
/// Returns `true` if the file content was changed, `false` otherwise.
pub fn apply_update(
    change_log: &ChangeLog,
    file: &File,
    modified_contents: &[u8],
    dry_run: bool,
    color_choice: ColorChoice,
) -> Result<bool, Error> {
    if *file.contents == *modified_contents {
        return Ok(false);
    }

    if dry_run {
        let original = String::from_utf8_lossy(&file.contents);
        let modified = String::from_utf8_lossy(modified_contents);
        let patch = diffy::create_patch(&original, &modified);
        let mut formatter = PatchFormatter::new();

        if should_use_colors(color_choice) {
            formatter = formatter.with_color();
        };

        println!("diff of '{}':", mago_bytes::BytesDisplay(&file.name));
        println!("{}", formatter.fmt_patch(&patch));
    } else {
        change_log.update(file.id, Cow::Owned(modified_contents.to_vec()))?;
    }

    Ok(true)
}
