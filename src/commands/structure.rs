use std::path::PathBuf;
use std::process::ExitCode;

use clap::ColorChoice;
use clap::Parser;

use mago_reporting::Level;
use mago_reporting::baseline::BaselineVariant;

use crate::commands::args::baseline_reporting::BaselineReportingArgs;
use crate::commands::stdin_input;
use crate::config::Configuration;
use crate::error::Error;
use crate::utils::create_orchestrator;

/// Emit the project's declarations, `use` imports and name references as the Lean module
/// `.mago/structure/Sharp.lean`, and prove every rules file `X.lean` that sits beside a folder `X/`.
#[derive(Parser, Debug)]
#[command(name = "structure")]
pub struct StructureCommand {
    /// Files or directories to collect instead of the configured source paths.
    #[arg()]
    pub path: Vec<PathBuf>,

    /// Print the module to stdout instead of checking the rules.
    #[arg(long, default_value_t = false)]
    pub print: bool,

    /// Arguments related to reporting issues with baseline support.
    #[clap(flatten)]
    pub baseline_reporting: BaselineReportingArgs,
}

impl StructureCommand {
    pub fn execute(self, mut configuration: Configuration, color_choice: ColorChoice) -> Result<ExitCode, Error> {
        let editor_url = configuration.editor_url.take();
        let mut orchestrator = create_orchestrator(&configuration, color_choice, false, true, false);
        stdin_input::set_source_paths_from_paths(&mut orchestrator, &self.path);

        let mut database = orchestrator.load_database(&configuration.source.workspace, false, None, None)?;
        let structure = orchestrator.get_structure_service(database.read_only()).run()?;

        if self.print {
            print!("{structure}");

            return Ok(ExitCode::SUCCESS);
        }

        let issues = structure.check(&configuration.source.workspace, &configuration.source.paths, &database)?;
        let processor = self.baseline_reporting.get_processor(
            color_choice,
            None,
            BaselineVariant::default(),
            editor_url,
            Level::Error,
            !self.path.is_empty(),
        );

        let (exit_code, _) = processor.process_issues(&orchestrator, &mut database, issues)?;

        Ok(exit_code)
    }
}
