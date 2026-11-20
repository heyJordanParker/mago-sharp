//! Machine command implementation.
//!
//! `mago machine <NAME>` prints the graph of the PHP# machine `NAME`, spec section 20.2, as a Mermaid state diagram:
//! an arrow from the start to the case each entry point gives, one from each case to each case a move from it gives,
//! each named by the method and its parameters, and one from each `final` case to the end.

use std::fmt::Write;
use std::process::ExitCode;

use clap::ColorChoice;
use clap::Parser;

use mago_allocator::LocalArena;
use mago_codex::metadata::machine::MachineMetadata;
use mago_codex::scanner::scan_program;
use mago_database::DatabaseReader;
use mago_names::resolver::NameResolver;
use mago_syntax::cst::ClassLikeMember;
use mago_syntax::cst::Method;
use mago_syntax::cst::Statement;
use mago_syntax::parser::parse_file_with_settings;
use mago_word::word;

use crate::config::Configuration;
use crate::error::Error;
use crate::utils::create_orchestrator;

/// Print a PHP# machine's graph as a Mermaid state diagram.
#[derive(Parser, Debug)]
#[command(
    name = "machine",
    about = "Print a PHP# machine's graph as a Mermaid state diagram.",
    long_about = "Print a PHP# machine's graph as a Mermaid state diagram.\n\n\
                  Draws an arrow from the start to the case each entry point gives, one from each case to each case \
                  a move from it gives, named by the move and its parameters, and one from each `final` case to the \
                  end."
)]
pub struct MachineCommand {
    /// The full name of the machine, as `App.Shop.OrderStatus`.
    name: String,
}

impl MachineCommand {
    /// Prints the graph of the machine the name names, from the PHP# file of the workspace that declares it.
    pub fn execute(self, configuration: Configuration, color_choice: ColorChoice) -> Result<ExitCode, Error> {
        let orchestrator = create_orchestrator(&configuration, color_choice, false, false, false);
        let database = orchestrator.load_database(&configuration.source.workspace, false, None, None)?;
        let name = self.name.trim_start_matches('\\').replace('.', "\\");

        for file in database.files().filter(|file| file.file_type.is_host() && file.name.ends_with(b".sharp")) {
            let arena = LocalArena::new();
            let program = parse_file_with_settings(&arena, &file, configuration.parser.to_settings());
            let resolved = NameResolver::new(&arena).resolve(program);
            let codebase = scan_program(&arena, &file, program, &resolved, configuration.php_version);
            let Some(machine) = codebase.get_class_like(word(&name)).and_then(|class| class.machine.as_ref()) else {
                continue;
            };
            let methods: Vec<&Method> = program
                .statements
                .iter()
                .flat_map(|statement| match statement {
                    Statement::Namespace(namespace) => namespace.statements().iter().collect(),
                    statement => vec![statement],
                })
                .filter_map(|statement| match statement {
                    Statement::Enum(r#enum) if r#enum.is_machine() => Some(r#enum),
                    _ => None,
                })
                .flat_map(|r#enum| r#enum.members.iter())
                .flat_map(|member| match member {
                    ClassLikeMember::EnumCase(case) => case.members().iter().collect(),
                    member => vec![member],
                })
                .filter_map(|member| match member {
                    ClassLikeMember::Method(method) => Some(method),
                    _ => None,
                })
                .collect();
            print!("{}", diagram(machine, &methods));

            return Ok(ExitCode::SUCCESS);
        }

        Err(Error::InvalidArgument(format!("The project declares no machine named `{}`.", self.name)))
    }
}

/// The Mermaid state diagram of `machine`, whose methods are among `methods`: each entry point and move is named by its
/// method and that method's parameters, as `pay(cents)`.
fn diagram(machine: &MachineMetadata, methods: &[&Method]) -> String {
    let written = |span| {
        methods.iter().find(|method| method.name.span == span).map_or_else(String::new, |method| {
            let parameters: Vec<String> = method
                .parameter_list
                .parameters
                .iter()
                .map(|parameter| {
                    let name = parameter.variable.name;
                    String::from_utf8_lossy(name.strip_prefix(b"$").unwrap_or(name)).into_owned()
                })
                .collect();

            format!("{}({})", String::from_utf8_lossy(method.name.value), parameters.join(", "))
        })
    };

    let mut diagram = String::from("stateDiagram-v2\n");
    for entry in &machine.entry_points {
        for to in &entry.to {
            let _ = writeln!(diagram, "    [*] --> {to}: {}", written(entry.span));
        }
    }
    for r#move in &machine.moves {
        for from in &r#move.from {
            for to in &r#move.to {
                let _ = writeln!(diagram, "    {from} --> {to}: {}", written(r#move.span));
            }
        }
    }
    for case in machine.cases.iter().filter(|case| case.is_final) {
        let _ = writeln!(diagram, "    {} --> [*]", case.name);
    }

    diagram
}
