use mago_database::file::File;
use mago_names::ResolvedNames;
use mago_span::Position;
use mago_span::Span;
use mago_syntax::cst::Program;
use mago_syntax::walker::MutWalker;

use crate::walker::StructureContext;
use crate::walker::StructureWalker;

pub mod error;

mod check;
mod lean;
mod walker;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Structure {
    pub declarations: Vec<Declaration>,
    pub imports: Vec<Import>,
    pub references: Vec<Reference>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    pub name: String,
    pub kind: DeclarationKind,
    pub location: Location,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    pub namespace: String,
    pub name: String,
    pub kind: SymbolKind,
    pub location: Location,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    pub namespace: String,
    pub name: String,
    pub location: Location,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclarationKind {
    Class,
    Interface,
    Trait,
    Enum,
    Function,
    Constant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    ClassLike,
    Function,
    Constant,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Location {
    pub file: String,
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

impl Structure {
    #[must_use]
    pub fn collect<'ast, 'arena>(
        file: &File,
        program: &'ast Program<'arena>,
        resolved_names: &'ast ResolvedNames<'arena>,
    ) -> Self {
        let mut context = StructureContext::new(file, resolved_names);
        StructureWalker.walk_program(program, &mut context);
        context.into_structure().sorted()
    }

    fn sorted(mut self) -> Self {
        self.declarations.sort_by(|a, b| a.location.cmp(&b.location));
        self.imports.sort_by(|a, b| a.location.cmp(&b.location));
        self.references.sort_by(|a, b| a.location.cmp(&b.location));
        self
    }
}

impl FromIterator<Structure> for Structure {
    fn from_iter<I>(structures: I) -> Self
    where
        I: IntoIterator<Item = Structure>,
    {
        let mut merged = Structure::default();
        for structure in structures {
            merged.declarations.extend(structure.declarations);
            merged.imports.extend(structure.imports);
            merged.references.extend(structure.references);
        }

        merged.sorted()
    }
}

impl Location {
    pub(crate) fn new(file: &File, start: u32, end: u32) -> Self {
        let last = end - 1;

        Self {
            file: String::from_utf8_lossy(&file.name).into_owned(),
            start_line: file.line_number(start) + 1,
            start_column: file.column_number(start) + 1,
            end_line: file.line_number(last) + 1,
            end_column: file.column_number(last) + 1,
        }
    }

    pub(crate) fn span(&self, file: &File) -> Option<Span> {
        let start = file.get_line_start_offset(self.start_line.checked_sub(1)?)?.checked_add(self.start_column)?;
        let end = file.get_line_start_offset(self.end_line.checked_sub(1)?)?.checked_add(self.end_column)?;

        (0 < start && start <= end && end <= file.size)
            .then(|| Span::new(file.id, Position::new(start - 1), Position::new(end)))
    }
}

pub(crate) fn lean_name(name: &[u8]) -> String {
    String::from_utf8_lossy(name.strip_prefix(b"\\").unwrap_or(name)).replace('\\', ".")
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use mago_allocator::LocalArena;
    use mago_names::resolver::NameResolver;
    use mago_syntax::parser::parse_file;

    use super::*;

    fn structure(code: &'static str) -> Structure {
        let arena = LocalArena::new();
        let file = File::ephemeral(Cow::Borrowed(b"src/test.php"), Cow::Borrowed(code.as_bytes()));
        let program = parse_file(&arena, &file);
        assert!(!program.has_errors(), "{:?}", program.errors);
        let resolved_names = NameResolver::new(&arena).resolve(program);

        Structure::collect(&file, program, &resolved_names)
    }

    fn location(start_line: u32, start_column: u32, end_line: u32, end_column: u32) -> Location {
        Location { file: "src/test.php".to_owned(), start_line, start_column, end_line, end_column }
    }

    #[test]
    fn names_are_dot_separated_without_a_leading_separator() {
        assert_eq!(lean_name(br"Illuminate\Support\Str"), "Illuminate.Support.Str");
        assert_eq!(lean_name(br"\Illuminate\Support\Str"), "Illuminate.Support.Str");
        assert_eq!(lean_name(b"strlen"), "strlen");
        assert_eq!(lean_name(b""), "");
    }

    #[test]
    fn locations_are_one_based_inclusive_byte_columns() {
        let file =
            File::ephemeral(Cow::Borrowed(b"src/test.php"), Cow::Borrowed("<?php\n$é = 1;\nfoo();\n".as_bytes()));

        assert_eq!(Location::new(&file, 0, 5), location(1, 1, 1, 5));
        assert_eq!(Location::new(&file, 6, 9), location(2, 1, 2, 3));
        assert_eq!(Location::new(&file, 15, 18), location(3, 1, 3, 3));
        assert_eq!(Location::new(&file, 6, 18), location(2, 1, 3, 3));
    }

    #[test]
    fn declares_every_named_class_like_function_and_constant() {
        let structure = structure(
            "<?php\nnamespace App;\nclass A {}\ninterface B {}\ntrait C {}\nenum D {}\nfunction e() {}\nconst F = 1, G = 2;\n$h = new class {};\n",
        );

        let declared: Vec<_> = structure.declarations.iter().map(|d| (d.name.as_str(), d.kind)).collect();
        assert_eq!(
            declared,
            [
                ("App.A", DeclarationKind::Class),
                ("App.B", DeclarationKind::Interface),
                ("App.C", DeclarationKind::Trait),
                ("App.D", DeclarationKind::Enum),
                ("App.e", DeclarationKind::Function),
                ("App.F", DeclarationKind::Constant),
                ("App.G", DeclarationKind::Constant),
            ]
        );
        assert_eq!(structure.declarations[0].location, location(3, 7, 3, 7));
        assert_eq!(structure.declarations[6].location, location(8, 14, 8, 14));
    }

    #[test]
    fn imports_every_use_item_with_its_kind() {
        let structure = structure(
            "<?php\nnamespace App;\nuse \\Lib\\A, Lib\\B as C;\nuse function Lib\\f;\nuse const Lib\\G;\nuse function Lib\\{h, i};\nuse Lib\\{J, function k, const L};\n",
        );

        let imported: Vec<_> =
            structure.imports.iter().map(|i| (i.namespace.as_str(), i.name.as_str(), i.kind)).collect();
        assert_eq!(
            imported,
            [
                ("App", "Lib.A", SymbolKind::ClassLike),
                ("App", "Lib.B", SymbolKind::ClassLike),
                ("App", "Lib.f", SymbolKind::Function),
                ("App", "Lib.G", SymbolKind::Constant),
                ("App", "Lib.h", SymbolKind::Function),
                ("App", "Lib.i", SymbolKind::Function),
                ("App", "Lib.J", SymbolKind::ClassLike),
                ("App", "Lib.k", SymbolKind::Function),
                ("App", "Lib.L", SymbolKind::Constant),
            ]
        );
        assert_eq!(structure.imports[0].location, location(3, 5, 3, 10));
        assert_eq!(structure.imports[8].location, location(7, 31, 7, 31));
        assert!(structure.references.is_empty(), "{:?}", structure.references);
    }

    #[test]
    fn references_every_other_resolved_name_in_its_namespace() {
        let structure = structure(
            "<?php\nnamespace App {\nuse Lib\\Money;\nclass Store extends Base { public function price(): Money { return format(new Money()); } }\n}\nnamespace {\nLib\\run();\n}\n",
        );

        let referenced: Vec<_> = structure
            .references
            .iter()
            .map(|r| (r.namespace.as_str(), r.name.as_str(), r.location.start_column))
            .collect();
        assert_eq!(
            referenced,
            [
                ("App", "App.Base", 21),
                ("App", "Lib.Money", 53),
                ("App", "App.format", 68),
                ("App", "Lib.Money", 79),
                ("", "Lib.run", 1),
            ]
        );
        assert_eq!(structure.references[4].location, location(7, 1, 7, 7));
    }

    #[test]
    fn merges_facts_sorted_by_file_line_and_column() {
        let b = Reference { namespace: String::new(), name: "B".to_owned(), location: location(2, 1, 2, 1) };
        let a = Reference { namespace: String::new(), name: "A".to_owned(), location: location(1, 9, 1, 9) };
        let mut other = a.clone();
        other.location.file = "src/other.php".to_owned();

        let merged: Structure = [
            Structure { references: vec![b.clone()], ..Structure::default() },
            Structure { references: vec![a.clone(), other.clone()], ..Structure::default() },
        ]
        .into_iter()
        .collect();

        assert_eq!(merged.references, [other, a, b]);
    }
}
