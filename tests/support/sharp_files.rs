//! The file a test's PHP# source is, as spec section 27 names a `.sharp` file: after the one type it declares.

use std::borrow::Cow;

use mago_allocator::LocalArena;
use mago_database::file::File;
use mago_syntax::cst::Statement;
use mago_syntax::parser::parse_file;

/// `src/<Type>.sharp` for the first type `code` declares at file level, as the parser reads it, or `src/Report.sharp`
/// when it declares none.
pub fn type_file(code: &str) -> String {
    let arena = LocalArena::new();
    let file = File::ephemeral(Cow::Borrowed(b"src/Source.sharp"), Cow::Owned(code.as_bytes().to_vec()));
    let program = parse_file(&arena, &file);
    let declared = |statement: &Statement| match statement {
        Statement::Class(class) => Some(class.name.span),
        Statement::Interface(interface) => Some(interface.name.span),
        Statement::Enum(r#enum) => Some(r#enum.name.span),
        Statement::Struct(r#struct) => Some(r#struct.name.span),
        Statement::Event(event) => Some(event.name.span),
        _ => None,
    };
    let name = program
        .statements
        .iter()
        .find_map(|statement| match statement {
            Statement::Namespace(namespace) => namespace.statements().iter().find_map(declared),
            statement => declared(statement),
        })
        .map_or("Report", |span| &code[span.start.offset as usize..span.end.offset as usize]);

    format!("src/{name}.sharp")
}
