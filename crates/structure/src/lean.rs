use std::fmt;
use std::fmt::Display;
use std::fmt::Formatter;
use std::fmt::Write;

use crate::Declaration;
use crate::DeclarationKind;
use crate::Import;
use crate::Location;
use crate::Reference;
use crate::Structure;
use crate::SymbolKind;

const CHUNK_SIZE: usize = 500;

struct Literal<'text>(&'text str);

impl Display for Structure {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        writeln!(f, "import Sharp.Facts")?;
        writeln!(f, "namespace Sharp")?;
        write_list(f, "declarations", "Declaration", &self.declarations)?;
        write_list(f, "imports", "Import", &self.imports)?;
        write_list(f, "references", "Reference", &self.references)?;
        writeln!(f, "end Sharp")
    }
}

fn write_list(f: &mut Formatter<'_>, name: &str, r#type: &str, facts: &[impl Display]) -> fmt::Result {
    if facts.len() > CHUNK_SIZE {
        let mut chunks = Vec::new();
        for (index, chunk) in facts.chunks(CHUNK_SIZE).enumerate() {
            let chunk_name = format!("{name}{index}");
            write_list(f, &chunk_name, r#type, chunk)?;
            chunks.push(chunk_name);
        }

        return writeln!(f, "def {name} : List {type} := {}", chunks.join(" ++ "));
    }

    writeln!(f, "def {name} : List {type} := [")?;
    for (index, fact) in facts.iter().enumerate() {
        let separator = if index + 1 == facts.len() { "" } else { "," };
        writeln!(f, "  {fact}{separator}")?;
    }

    writeln!(f, "]")
}

impl Display for Declaration {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "⟨{}, .{}, {}⟩", Literal(&self.name), self.kind, self.location)
    }
}

impl Display for Import {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "⟨{}, {}, .{}, {}⟩", Literal(&self.namespace), Literal(&self.name), self.kind, self.location)
    }
}

impl Display for Reference {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "⟨{}, {}, {}⟩", Literal(&self.namespace), Literal(&self.name), self.location)
    }
}

impl Display for Location {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "⟨{}, {}, {}, {}, {}⟩",
            Literal(&self.file),
            self.start_line,
            self.start_column,
            self.end_line,
            self.end_column
        )
    }
}

impl Display for DeclarationKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Class => "class",
            Self::Interface => "interface",
            Self::Trait => "trait",
            Self::Enum => "enum",
            Self::Function => "function",
            Self::Constant => "constant",
        })
    }
}

impl Display for SymbolKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ClassLike => "classLike",
            Self::Function => "function",
            Self::Constant => "constant",
        })
    }
}

impl Display for Literal<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_char('"')?;
        for character in self.0.chars() {
            match character {
                '"' => f.write_str("\\\"")?,
                '\\' => f.write_str("\\\\")?,
                '\n' => f.write_str("\\n")?,
                '\r' => f.write_str("\\r")?,
                '\t' => f.write_str("\\t")?,
                control if control.is_ascii_control() => write!(f, "\\x{:02x}", control as u32)?,
                other => f.write_char(other)?,
            }
        }

        f.write_char('"')
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_lean_string_literals() {
        assert_eq!(Literal("App.Shared.Money").to_string(), r#""App.Shared.Money""#);
        assert_eq!(Literal("a\"b\\c").to_string(), r#""a\"b\\c""#);
        assert_eq!(Literal("a\nb\r\tc\u{0}\u{7f}é").to_string(), r#""a\nb\r\tc\x00\x7fé""#);
    }

    #[test]
    fn renders_every_list_with_separators_between_facts() {
        let location =
            Location { file: "src/a.php".to_owned(), start_line: 1, start_column: 2, end_line: 3, end_column: 4 };
        let structure = Structure {
            declarations: vec![
                Declaration { name: "A".to_owned(), kind: DeclarationKind::Trait, location: location.clone() },
                Declaration { name: "b".to_owned(), kind: DeclarationKind::Constant, location: location.clone() },
            ],
            imports: vec![Import {
                namespace: String::new(),
                name: "c".to_owned(),
                kind: SymbolKind::Function,
                location,
            }],
            references: Vec::new(),
        };

        assert_eq!(
            structure.to_string(),
            r#"import Sharp.Facts
namespace Sharp
def declarations : List Declaration := [
  ⟨"A", .trait, ⟨"src/a.php", 1, 2, 3, 4⟩⟩,
  ⟨"b", .constant, ⟨"src/a.php", 1, 2, 3, 4⟩⟩
]
def imports : List Import := [
  ⟨"", "c", .function, ⟨"src/a.php", 1, 2, 3, 4⟩⟩
]
def references : List Reference := [
]
end Sharp
"#
        );
    }

    #[test]
    fn splits_a_list_longer_than_500_facts_into_definitions_joined_under_its_name() {
        let reference = Reference {
            namespace: "App".to_owned(),
            name: "B".to_owned(),
            location: Location {
                file: "src/a.php".to_owned(),
                start_line: 1,
                start_column: 2,
                end_line: 3,
                end_column: 4,
            },
        };
        let fact = r#"  ⟨"App", "B", ⟨"src/a.php", 1, 2, 3, 4⟩⟩"#;
        let five_hundred = vec![fact; 500].join(",\n");

        let at_limit = Structure { references: vec![reference.clone(); 500], ..Structure::default() };
        assert_eq!(
            at_limit.to_string(),
            format!(
                "import Sharp.Facts\nnamespace Sharp\ndef declarations : List Declaration := [\n]\ndef imports : List Import := [\n]\ndef references : List Reference := [\n{five_hundred}\n]\nend Sharp\n"
            )
        );

        let past_limit = Structure { references: vec![reference; 501], ..Structure::default() };
        assert_eq!(
            past_limit.to_string(),
            format!(
                "import Sharp.Facts\nnamespace Sharp\ndef declarations : List Declaration := [\n]\ndef imports : List Import := [\n]\ndef references0 : List Reference := [\n{five_hundred}\n]\ndef references1 : List Reference := [\n{fact}\n]\ndef references : List Reference := references0 ++ references1\nend Sharp\n"
            )
        );
    }
}
