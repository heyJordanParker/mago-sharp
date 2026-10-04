use mago_database::ReadDatabase;
use mago_names::resolver::NameResolver;
use mago_structure::Structure;
use mago_syntax::parser::parse_file_with_settings;
use mago_syntax::settings::ParserSettings;

use crate::error::OrchestratorError;
use crate::service::pipeline::StatelessParallelPipeline;
use crate::service::pipeline::StatelessReducer;

#[derive(Debug)]
pub struct StructureService {
    database: ReadDatabase,
    parser_settings: ParserSettings,
    use_progress_bars: bool,
}

impl StructureService {
    #[must_use]
    pub fn new(database: ReadDatabase, parser_settings: ParserSettings, use_progress_bars: bool) -> Self {
        Self { database, parser_settings, use_progress_bars }
    }

    /// # Errors
    ///
    /// Returns [`OrchestratorError::General`] naming the file and its first parse error when a host file
    /// does not parse, because facts from a partial syntax tree would let a rule pass over code it never saw.
    pub fn run(self) -> Result<Structure, OrchestratorError> {
        const STRUCTURE_PROGRESS_PREFIX: &str = "🧱 Structuring";

        let pipeline = StatelessParallelPipeline::new(
            STRUCTURE_PROGRESS_PREFIX,
            self.database,
            self.parser_settings,
            Box::new(StructureReducer),
            self.use_progress_bars,
        );

        pipeline.run(|parser_settings, arena, source_file| {
            let program = parse_file_with_settings(arena, &source_file, parser_settings);
            if let Some(error) = program.errors.first() {
                return Err(OrchestratorError::General(format!(
                    "Cannot collect the structure of `{}`: {error}",
                    String::from_utf8_lossy(&source_file.name)
                )));
            }

            let resolved_names = NameResolver::new(arena).resolve(program);

            Ok(Structure::collect(&source_file, program, &resolved_names))
        })
    }
}

#[derive(Debug, Clone)]
struct StructureReducer;

impl StatelessReducer<Structure, Structure> for StructureReducer {
    fn reduce(&self, results: Vec<Structure>) -> Result<Structure, OrchestratorError> {
        Ok(results.into_iter().collect())
    }
}
