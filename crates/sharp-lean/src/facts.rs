//! One `.sharp` file's facts, as structure rules read them: its declarations, its imports and its name references.
//!
//! They come from the same `CheckedProgram` the translation reads. A name the file declares or imports is that fact
//! alone, and every other name the analysis resolved in it is a reference.

use foldhash::HashSet;
use mago_database::file::File;
use mago_names::ResolvedNames;
use mago_sharp_bridge::CheckedProgram;
use mago_span::HasSpan;
use mago_span::Position;
use mago_span::Span;
use mago_syntax::cst::Class;
use mago_syntax::cst::Enum;
use mago_syntax::cst::Interface;
use mago_syntax::cst::Module;
use mago_syntax::cst::Namespace;
use mago_syntax::cst::Struct;
use mago_syntax::cst::UseItem;
use mago_syntax::walker::MutWalker;

/// The facts of one `.sharp` file.
#[derive(Debug)]
pub(crate) struct Facts {
    /// The namespace of the file's code, dot separated. "" is the global namespace.
    pub(crate) namespace: String,
    /// Where an issue about the whole file sits: its namespace's name, else its start.
    pub(crate) anchor: Span,
    /// In source order.
    pub(crate) list: Vec<Fact>,
}

#[derive(Debug)]
pub(crate) struct Fact {
    pub(crate) kind: Kind,
    /// Fully qualified and dot separated: `App.Shared.Money`.
    pub(crate) name: String,
    pub(crate) span: Span,
    pub(crate) location: Location,
}

#[derive(Debug)]
pub(crate) enum Kind {
    Declaration(DeclarationKind),
    /// The namespace whose code holds the import of a class-like, the only symbol a `.sharp` file imports.
    Import(String),
    /// The namespace whose code holds the reference.
    Reference(String),
}

/// As `Sharp.DeclarationKind` numbers them: what a `.sharp` file declares.
#[derive(Debug, Clone, Copy)]
pub(crate) enum DeclarationKind {
    Class = 0,
    Interface = 1,
    Enum = 2,
}

/// Where a fact sits, as `Sharp.Location` holds it: 1-based and inclusive, with columns counting bytes.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Location {
    pub(crate) start_line: u32,
    pub(crate) start_column: u32,
    pub(crate) end_line: u32,
    pub(crate) end_column: u32,
}

impl Fact {
    /// What the fact is, as a message names it: `import of App.Tenant.Store.Receipts`.
    pub(crate) fn describe(&self) -> String {
        match self.kind {
            Kind::Declaration(_) => format!("declaration of {}", self.name),
            Kind::Import(..) => format!("import of {}", self.name),
            Kind::Reference(_) => format!("reference to {}", self.name),
        }
    }
}

/// The facts of `checked`.
pub(crate) fn collect(checked: &CheckedProgram<'_>) -> Facts {
    let file = checked.file();
    let mut collector = Collector {
        file,
        names: checked.names(),
        namespace: String::new(),
        namespaces: Vec::new(),
        claimed: HashSet::default(),
        facts: Vec::new(),
    };
    Walk.walk_program(checked.program(), &mut collector);

    let references: Vec<Fact> = collector
        .names
        .iter()
        .filter(|(start, ..)| !collector.claimed.contains(start))
        .map(|(start, end, name, _)| Fact {
            kind: Kind::Reference(collector.namespace_at(start)),
            name: dotted(name),
            span: Span::new(file.id, Position::new(start), Position::new(end)),
            location: location(file, start, end),
        })
        .collect();
    let mut list = collector.facts;
    list.extend(references);
    list.sort_by_key(|fact| fact.span.start.offset);

    let (anchor, namespace) = collector
        .namespaces
        .into_iter()
        .next()
        .map_or((Span::new(file.id, Position::new(0), Position::new(0)), String::new()), |(_, name, namespace)| {
            (name, namespace)
        });

    Facts { namespace, anchor, list }
}

struct Walk;

struct Collector<'input, 'arena> {
    file: &'input File,
    names: &'input ResolvedNames<'arena>,
    namespace: String,
    /// Each namespace statement: the code it holds, its name, and the namespace.
    namespaces: Vec<(Span, Span, String)>,
    /// The start of each resolved name that is a declaration or an import, and so no reference.
    claimed: HashSet<u32>,
    facts: Vec<Fact>,
}

impl Collector<'_, '_> {
    fn namespace_at(&self, offset: u32) -> String {
        self.namespaces
            .iter()
            .find(|(span, ..)| span.start.offset <= offset && offset < span.end.offset)
            .map(|(.., namespace)| namespace.clone())
            .unwrap_or_default()
    }

    fn add(&mut self, span: Span, kind: Kind) {
        self.claimed.insert(span.start.offset);
        self.facts.push(Fact {
            kind,
            name: dotted(self.names.get(&span)),
            span,
            location: location(self.file, span.start.offset, span.end.offset),
        });
    }

    fn declare(&mut self, name: &impl HasSpan, kind: DeclarationKind) {
        self.add(name.span(), Kind::Declaration(kind));
    }
}

impl<'ast, 'input, 'arena> MutWalker<'ast, 'arena, Collector<'input, 'arena>> for Walk {
    fn walk_in_namespace(&mut self, namespace: &'ast Namespace<'arena>, collector: &mut Collector<'input, 'arena>) {
        collector.namespace = namespace.name.as_ref().map(|name| dotted(name.value())).unwrap_or_default();
        let name = namespace.name.as_ref().map_or_else(|| namespace.namespace.span(), HasSpan::span);
        collector.namespaces.push((namespace.span(), name, collector.namespace.clone()));
        collector.claimed.insert(name.start.offset);
    }

    fn walk_out_namespace(&mut self, _namespace: &'ast Namespace<'arena>, collector: &mut Collector<'input, 'arena>) {
        collector.namespace.clear();
    }

    /// Each name of an `import`, the only form of `use` a `.sharp` file writes.
    fn walk_in_use_item(&mut self, item: &'ast UseItem<'arena>, collector: &mut Collector<'input, 'arena>) {
        collector.add(item.name.span(), Kind::Import(collector.namespace.clone()));
    }

    fn walk_in_class(&mut self, class: &'ast Class<'arena>, collector: &mut Collector<'input, 'arena>) {
        collector.declare(&class.name, DeclarationKind::Class);
    }

    /// A struct is the `final` class it lowers to, spec section 10.
    fn walk_in_struct(&mut self, r#struct: &'ast Struct<'arena>, collector: &mut Collector<'input, 'arena>) {
        collector.declare(&r#struct.name, DeclarationKind::Class);
    }

    fn walk_in_interface(&mut self, interface: &'ast Interface<'arena>, collector: &mut Collector<'input, 'arena>) {
        collector.declare(&interface.name, DeclarationKind::Interface);
    }

    fn walk_in_enum(&mut self, r#enum: &'ast Enum<'arena>, collector: &mut Collector<'input, 'arena>) {
        collector.declare(&r#enum.name, DeclarationKind::Enum);
    }

    /// A module's name is its namespace's `Module` class-like, which the file declares by its name.
    fn walk_in_module(&mut self, module: &'ast Module<'arena>, collector: &mut Collector<'input, 'arena>) {
        collector.claimed.insert(module.module.span().start.offset);
    }
}

/// The location of the bytes from `start` to `end` in `file`.
fn location(file: &File, start: u32, end: u32) -> Location {
    let last = end.saturating_sub(1).max(start);

    Location {
        start_line: file.line_number(start) + 1,
        start_column: file.column_number(start) + 1,
        end_line: file.line_number(last) + 1,
        end_column: file.column_number(last) + 1,
    }
}

/// A PHP name as the facts hold it: dot separated, with no leading separator.
fn dotted(name: &[u8]) -> String {
    String::from_utf8_lossy(name.strip_prefix(b"\\").unwrap_or(name)).replace('\\', ".")
}
