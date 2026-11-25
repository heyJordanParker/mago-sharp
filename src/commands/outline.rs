//! `mago outline`: the symbols, imports and outbound references of one file, as JSON in the Language Server
//! Protocol's `DocumentSymbol` shape, from its text alone.
//!
//! It parses the file and resolves its names with the file's own namespace and imports, and reads no other file, no
//! project configuration and no database, so a tool that indexes or navigates files can run it on each file as it
//! changes. [`outline`] builds the whole answer from one file, for any caller.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use rayon::ThreadPoolBuilder;
use rayon::iter::IntoParallelRefIterator;
use rayon::iter::ParallelIterator;
use serde::Serialize;

use mago_allocator::LocalArena;
use mago_database::file::File;
use mago_linter::rule::cyclomatic_complexity;
use mago_names::ResolvedNames;
use mago_names::binding::Binding;
use mago_names::binding::php_method_name;
use mago_names::resolver::NameResolver;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::AnonymousClass;
use mago_syntax::cst::ArrowFunction;
use mago_syntax::cst::AttributeList;
use mago_syntax::cst::Call;
use mago_syntax::cst::Class;
use mago_syntax::cst::ClassConstantAccess;
use mago_syntax::cst::ClassLikeConstant;
use mago_syntax::cst::ClassLikeConstantSelector;
use mago_syntax::cst::ClassLikeMember;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Closure;
use mago_syntax::cst::ComputedProperty;
use mago_syntax::cst::Constant;
use mago_syntax::cst::Enum;
use mago_syntax::cst::EnumCase;
use mago_syntax::cst::EnumCaseBody;
use mago_syntax::cst::EnumCaseItem;
use mago_syntax::cst::Event;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Function;
use mago_syntax::cst::FunctionCall;
use mago_syntax::cst::FunctionLikeParameter;
use mago_syntax::cst::FunctionLikeReturnTypeHint;
use mago_syntax::cst::Hint;
use mago_syntax::cst::HookedProperty;
use mago_syntax::cst::Identifier;
use mago_syntax::cst::Indexer;
use mago_syntax::cst::Inheritance;
use mago_syntax::cst::Instantiation;
use mago_syntax::cst::Interface;
use mago_syntax::cst::Law;
use mago_syntax::cst::LocalDeclaration;
use mago_syntax::cst::LocalTarget;
use mago_syntax::cst::Method;
use mago_syntax::cst::MethodCall;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::Module;
use mago_syntax::cst::NamedConstructor;
use mago_syntax::cst::Namespace;
use mago_syntax::cst::Node;
use mago_syntax::cst::NullSafeMethodCall;
use mago_syntax::cst::Operator;
use mago_syntax::cst::PlainProperty;
use mago_syntax::cst::Program;
use mago_syntax::cst::PropertyAccess;
use mago_syntax::cst::Sequence;
use mago_syntax::cst::StaticMethodCall;
use mago_syntax::cst::Struct;
use mago_syntax::cst::Trait;
use mago_syntax::cst::TypeOf;
use mago_syntax::cst::TypeParameterList;
use mago_syntax::cst::Use;
use mago_syntax::cst::UseItem;
use mago_syntax::cst::UseItems;
use mago_syntax::cst::Variable;
use mago_syntax::cst::built_in_generic_arity;
use mago_syntax::dialect::Dialect;
use mago_syntax::parser::parse_file;
use mago_syntax::walker::MutWalker;
use mago_syntax::walker::walk_program_mut;

use crate::commands::stdin_input::resolve_stdin_override;
use crate::error::Error;

/// The shape's version. It rises with any change to the shape, and a version only gains fields.
const VERSION: u32 = 1;

/// Print the outline of each file: its symbols, imports and outbound references, as JSON.
#[derive(Parser, Debug)]
#[command(
    name = "outline",
    about = "Print each file's symbols, imports and references, from its text alone.",
    long_about = "Print each file's symbols as an indented tree, each with its kind, name and position, then its \
                  parse errors, from the file's text alone. `--json` prints its symbols, imports and outbound \
                  references in the Language Server Protocol's document-symbol shape, one object per line in the \
                  order given.\n\n\
                  It reads no other file and no project configuration. A file with syntax errors prints what the \
                  parser recovered, with its errors. It exits with an error only for a file it can't read."
)]
pub struct OutlineCommand {
    /// The files to outline. A `.sharp` file is PHP#, and any other is PHP.
    #[arg(required = true)]
    pub paths: Vec<PathBuf>,

    /// Print each file's outline as JSON, one object per line.
    #[arg(long)]
    pub json: bool,

    /// Read the file content from stdin and use the given path for it.
    ///
    /// Intended for editor integrations: pipe unsaved buffer content and pass the real file path.
    #[arg(long)]
    pub stdin_input: bool,
}

impl OutlineCommand {
    /// Prints the outline of each file, in the order given, the files outlined in parallel on `threads`, or the one
    /// `--stdin-input` reads, labelled with its one path, relative to the folder the command runs in. It runs before
    /// any logger starts, so it prints each reason it fails itself.
    pub fn execute(&self, threads: Option<usize>) -> ExitCode {
        match self.outline_all(threads) {
            Ok(exit_code) => exit_code,
            Err(error) => {
                eprintln!("mago outline: {error}");

                ExitCode::from(2)
            }
        }
    }

    fn outline_all(&self, threads: Option<usize>) -> Result<ExitCode, Error> {
        if let Some((name, contents)) = resolve_stdin_override(self.stdin_input, &self.paths, Path::new("."))? {
            print!("{}", self.render(&File::ephemeral(Cow::Owned(name.into_bytes()), Cow::Owned(contents)))?);

            return Ok(ExitCode::SUCCESS);
        }

        let read = |path: &PathBuf| {
            let name = path.to_string_lossy().into_owned().into_bytes();
            std::fs::read(path)
                .map(|contents| self.render(&File::ephemeral(Cow::Owned(name), Cow::Owned(contents))))
                .map_err(|error| format!("mago outline can't read {}: {error}", path.display()))
        };
        let outlines: Vec<_> = match self.paths.as_slice() {
            [path] => vec![read(path)],
            paths => ThreadPoolBuilder::new()
                .num_threads(threads.unwrap_or_default())
                .build()?
                .install(|| paths.par_iter().map(read).collect()),
        };

        let mut exit_code = ExitCode::SUCCESS;
        for outline in outlines {
            match outline {
                Ok(outline) => print!("{}", outline?),
                Err(reason) => {
                    eprintln!("{reason}");
                    exit_code = ExitCode::FAILURE;
                }
            }
        }

        Ok(exit_code)
    }

    /// The outline of `file` as `--json` asks: a line of JSON, or the indented tree.
    fn render(&self, file: &File) -> Result<String, Error> {
        let outline = outline(file);

        Ok(if self.json { format!("{}\n", serde_json::to_string(&outline)?) } else { outline.tree() })
    }
}

#[derive(Serialize)]
pub struct Outline {
    version: u32,
    language: &'static str,
    path: String,
    namespace: Option<String>,
    errors: Vec<SyntaxError>,
    pub symbols: Vec<Symbol>,
    imports: Vec<Import>,
    references: Vec<Reference>,
}

impl Outline {
    /// The file's path, then each symbol on a line of its own, as its kind, its name and where its range starts, each
    /// child indented under its parent, then each parse error with where it starts.
    fn tree(&self) -> String {
        fn write_symbols(tree: &mut String, symbols: &[Symbol], depth: usize) {
            for symbol in symbols {
                let start = symbol.range.start;
                let _ =
                    writeln!(tree, "{:depth$}{} {} {}:{}", "", symbol.kind, symbol.name, start.line, start.character);
                write_symbols(tree, &symbol.children, depth + 2);
            }
        }

        let mut tree = format!("{}\n", self.path);
        write_symbols(&mut tree, &self.symbols, 2);
        for error in &self.errors {
            let start = error.range.start;
            let _ = writeln!(tree, "  error {}:{} {}", start.line, start.character, error.message);
        }

        tree
    }
}

/// A place in a file as the Language Server Protocol counts it: a 0-based line, and the UTF-16 code units before it on
/// the line.
#[derive(Serialize, Clone, Copy)]
struct Position {
    line: u32,
    character: u32,
}

#[derive(Serialize, Clone, Copy)]
struct Range {
    start: Position,
    end: Position,
}

#[derive(Serialize)]
struct SyntaxError {
    range: Range,
    message: String,
}

/// A declaration, as the Language Server Protocol's `DocumentSymbol`, with the members it holds as its children.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Symbol {
    name: String,
    kind: Kind,
    detail: String,
    range: Range,
    selection_range: Range,
    visibility: Visibility,
    supertypes: Vec<String>,
    attributes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    complexity: Option<usize>,
    /// What a function, a method or a constructor returns, as [`Outliner::declared_type`] names it.
    #[serde(skip_serializing_if = "Option::is_none")]
    return_type: Option<Option<String>>,
    /// What a property, a field or a constant holds, as [`Outliner::declared_type`] names it.
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    value_type: Option<Option<String>>,
    children: Vec<Symbol>,
}

/// The Language Server Protocol's `SymbolKind` a declaration takes, in lower camel case. A PHP# construct the
/// protocol lacks takes the nearest one: a trait and an event are a `class`, a module a `namespace`, an indexer a
/// `property`, a case an `enumMember`, a named constructor a `constructor`, and an operator and a law a `method`. A
/// property with no accessors and no body is a `field`. A type parameter is a `typeParameter` child of what declares
/// it.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, strum::Display)]
#[serde(rename_all = "camelCase")]
#[strum(serialize_all = "camelCase")]
enum Kind {
    Namespace,
    Class,
    Interface,
    Enum,
    Struct,
    Method,
    Constructor,
    Function,
    Property,
    Field,
    Constant,
    EnumMember,
    TypeParameter,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum Visibility {
    Public,
    Protected,
    Private,
    Internal,
}

#[derive(Serialize)]
struct Import {
    path: String,
    alias: Option<String>,
    range: Range,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Reference {
    #[serde(skip)]
    offset: u32,
    /// Where the name of the call whose result this call is made on starts, as Roslyn's `IInvocationOperation`
    /// gives its `Instance`.
    #[serde(skip)]
    receiver_offset: Option<u32>,
    name: String,
    kind: ReferenceKind,
    range: Range,
    receiver_type: Option<String>,
    target: Option<String>,
    /// For a call, the index in `references` of the call whose result it is made on, or `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    receiver_call: Option<Option<usize>>,
}

/// An outbound use: a `call`, as `f()`, `x.f()` and `T.f()`, a `new T`, or a `type`, as `T::class` and `typeof(T)`.
#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum ReferenceKind {
    Call,
    New,
    Type,
}

/// The outline of `file`: its symbols, imports and outbound references, from its text alone.
pub fn outline(file: &File) -> Outline {
    let arena = LocalArena::new();
    let program = parse_file(&arena, file);
    let names = NameResolver::new(&arena).resolve(program);

    let mut outliner = Outliner::new(file, &names);
    walk_program_mut(&mut outliner, program, &mut ());
    outliner.references.sort_by_key(|reference| reference.offset);
    let calls: HashMap<u32, usize> = outliner
        .references
        .iter()
        .enumerate()
        .filter(|(_, reference)| matches!(reference.kind, ReferenceKind::Call))
        .map(|(index, reference)| (reference.offset, index))
        .collect();
    for reference in &mut outliner.references {
        if matches!(reference.kind, ReferenceKind::Call) {
            reference.receiver_call = Some(reference.receiver_offset.and_then(|offset| calls.get(&offset).copied()));
        }
    }

    Outline {
        version: VERSION,
        language: if outliner.sharp { "sharp" } else { "php" },
        path: String::from_utf8_lossy(&file.name).into_owned(),
        namespace: outliner.file_namespace.clone(),
        errors: errors(&outliner, program),
        symbols: tree(outliner.symbols),
        imports: outliner.imports,
        references: outliner.references,
    }
}

fn errors(outliner: &Outliner<'_, '_>, program: &Program<'_>) -> Vec<SyntaxError> {
    program
        .errors
        .iter()
        .map(|error| SyntaxError { range: outliner.range(error.span()), message: error.to_string() })
        .collect()
}

/// Where the name of the call `expression` is starts, when it is a call: the name its reference covers.
fn call_name(expression: &Expression<'_>) -> Option<u32> {
    let name = match expression {
        Expression::Parenthesized(parenthesized) => return call_name(parenthesized.expression),
        Expression::Call(Call::Function(FunctionCall { function: Expression::Identifier(function), .. })) => {
            return Some(function.span().start.offset);
        }
        Expression::Call(Call::Method(MethodCall { method, .. }))
        | Expression::Call(Call::NullSafeMethod(NullSafeMethodCall { method, .. }))
        | Expression::Call(Call::StaticMethod(StaticMethodCall { method, .. })) => method,
        _ => return None,
    };
    match name {
        ClassLikeMemberSelector::Identifier(name) => Some(name.span.start.offset),
        _ => None,
    }
}

/// Nests each symbol in the one that holds it. A symbol comes after the one that holds it, so the last one is a leaf.
fn tree(mut symbols: Vec<(Option<usize>, Symbol)>) -> Vec<Symbol> {
    let mut roots = Vec::new();
    while let Some((parent, symbol)) = symbols.pop() {
        match parent {
            Some(parent) => symbols[parent].1.children.insert(0, symbol),
            None => roots.insert(0, symbol),
        }
    }

    roots
}

/// A declaration that holds members, or a class whose members are no symbols of their own: an anonymous class.
struct Scope<'ast, 'arena> {
    symbol: Option<usize>,
    visibility: Visibility,
    interface: bool,
    /// What `this` is in its methods, for a class-like: the class's full name, or `None` in an anonymous class.
    class: Option<Option<String>>,
    base: Option<String>,
    properties: HashMap<&'arena [u8], &'ast Hint<'arena>>,
}

struct Outliner<'ast, 'arena> {
    file: &'ast File,
    names: &'ast ResolvedNames<'arena>,
    sharp: bool,
    file_namespace: Option<String>,
    namespace: Option<String>,
    scopes: Vec<Scope<'ast, 'arena>>,
    /// The type each PHP# parameter and typed local is declared with, by the offset of its name, as a binding names it.
    locals: HashMap<u32, &'ast Hint<'arena>>,
    /// The type each PHP parameter is declared with, by its name, in each function that encloses the code.
    parameters: Vec<HashMap<&'arena [u8], &'ast Hint<'arena>>>,
    /// Each symbol, after the one that holds it, with that one's index.
    symbols: Vec<(Option<usize>, Symbol)>,
    imports: Vec<Import>,
    references: Vec<Reference>,
}

impl<'ast, 'arena> Outliner<'ast, 'arena> {
    fn new(file: &'ast File, names: &'ast ResolvedNames<'arena>) -> Self {
        Self {
            file,
            names,
            sharp: Dialect::of(file).is_sharp(),
            file_namespace: None,
            namespace: None,
            scopes: Vec::new(),
            locals: HashMap::new(),
            parameters: Vec::new(),
            symbols: Vec::new(),
            imports: Vec::new(),
            references: Vec::new(),
        }
    }

    fn position(&self, offset: u32) -> Position {
        let line = self.file.line_number(offset);
        let start = self.file.lines[line as usize] as usize;
        let before = String::from_utf8_lossy(&self.file.contents[start..offset as usize]);

        Position { line, character: before.encode_utf16().count() as u32 }
    }

    fn range(&self, span: Span) -> Range {
        Range { start: self.position(span.start.offset), end: self.position(span.end.offset) }
    }

    fn text(&self, start: u32, end: u32) -> String {
        String::from_utf8_lossy(&self.file.contents[start as usize..end as usize]).trim_end().to_owned()
    }

    /// A name the resolver gives, in PHP's form, in the file's language: `Paging.Order` in PHP#.
    fn display(&self, name: &[u8]) -> String {
        let name = String::from_utf8_lossy(name.strip_prefix(b"\\").unwrap_or(name)).into_owned();

        if self.sharp { name.replace('\\', ".") } else { name }
    }

    fn qualified(&self, name: &str) -> String {
        match &self.namespace {
            Some(namespace) => format!("{namespace}{}{name}", if self.sharp { "." } else { "\\" }),
            None => name.to_owned(),
        }
    }

    fn member(&self, class: &str, member: &str) -> String {
        format!("{class}{}{member}", if self.sharp { "." } else { "::" })
    }

    /// The full name of the class `identifier` names, or `None` for a type parameter.
    fn class_name(&self, identifier: &impl HasSpan) -> Option<String> {
        if self.names.is_type_parameter(identifier) {
            return None;
        }

        self.names.resolve(identifier).map(|name| self.display(name))
    }

    /// The type a declared type names, a type the project, a library or the standard library declares, by its full
    /// name. A built-in type, a type parameter and a type that joins several name none, as SCIP gives a built-in no
    /// symbol.
    fn type_name(&self, hint: &Hint<'_>) -> Option<String> {
        match hint {
            Hint::Identifier(identifier) => self.class_name(identifier),
            Hint::Generic(generic) if built_in_generic_arity(generic.name.value).is_none() => {
                self.class_name(&generic.name)
            }
            Hint::Nullable(nullable) => self.type_name(nullable.hint),
            Hint::Parenthesized(parenthesized) => self.type_name(parenthesized.hint),
            Hint::Static(_) | Hint::Self_(_) => self.this(),
            _ => None,
        }
    }

    /// The type a declaration's type names, which a member call on its value resolves against: [`Self::type_name`]'s,
    /// or a type parameter by its name.
    fn declared_type(&self, hint: &Hint<'_>) -> Option<String> {
        match hint {
            Hint::Identifier(identifier) if self.names.is_type_parameter(identifier) => {
                Some(String::from_utf8_lossy(identifier.value()).into_owned())
            }
            Hint::Nullable(nullable) => self.declared_type(nullable.hint),
            Hint::Parenthesized(parenthesized) => self.declared_type(parenthesized.hint),
            hint => self.type_name(hint),
        }
    }

    fn set_return_type(&mut self, symbol: Option<usize>, hint: Option<&FunctionLikeReturnTypeHint<'_>>) {
        let return_type = hint.and_then(|hint| self.declared_type(&hint.hint));
        if let Some(index) = symbol {
            self.symbols[index].1.return_type = Some(return_type);
        }
    }

    fn set_type(&mut self, symbol: Option<usize>, hint: Option<&Hint<'_>>) {
        let value_type = hint.and_then(|hint| self.declared_type(hint));
        if let Some(index) = symbol {
            self.symbols[index].1.value_type = Some(value_type);
        }
    }

    /// Adds each type parameter `list` declares as a child of `owner`, which reaches as far as its owner.
    fn declare_type_parameters(&mut self, owner: Option<usize>, list: Option<&TypeParameterList<'_>>) {
        let (Some(owner), Some(list)) = (owner, list) else {
            return;
        };
        let visibility = self.symbols[owner].1.visibility;
        for parameter in list.parameters.iter() {
            let span = parameter.span();
            let symbol = Symbol {
                name: String::from_utf8_lossy(parameter.name.value).into_owned(),
                kind: Kind::TypeParameter,
                detail: self.text(span.start.offset, span.end.offset),
                range: self.range(span),
                selection_range: self.range(parameter.name.span),
                visibility,
                supertypes: Vec::new(),
                attributes: Vec::new(),
                complexity: None,
                return_type: None,
                value_type: None,
                children: Vec::new(),
            };
            self.symbols.push((Some(owner), symbol));
        }
    }

    fn class_scope(&self) -> Option<&Scope<'ast, 'arena>> {
        self.scopes.iter().rev().find(|scope| scope.class.is_some())
    }

    fn this(&self) -> Option<String> {
        self.class_scope().and_then(|scope| scope.class.clone().flatten())
    }

    fn property_type(&self, name: &[u8]) -> Option<String> {
        self.class_scope().and_then(|scope| scope.properties.get(name)).and_then(|hint| self.type_name(hint))
    }

    fn is_this(&self, expression: &Expression<'_>) -> bool {
        match expression {
            Expression::ConstantAccess(access) => matches!(self.names.binding(&access.name), Some(Binding::This)),
            Expression::Variable(Variable::Direct(variable)) => variable.name == b"$this",
            _ => false,
        }
    }

    /// The type of a call's receiver, when the file declares it: a parameter, a typed local, a property or `this`.
    fn receiver_type(&self, receiver: &Expression<'_>) -> Option<String> {
        match receiver {
            Expression::ConstantAccess(access) => match self.names.binding(&access.name)? {
                Binding::Local(local) => self.type_name(self.locals.get(&local.declaration.start.offset)?),
                Binding::This => self.this(),
                Binding::Member => self.property_type(access.name.value()),
                _ => None,
            },
            Expression::Variable(Variable::Direct(variable)) if variable.name == b"$this" => self.this(),
            Expression::Variable(Variable::Direct(variable)) => {
                self.type_name(self.parameters.last()?.get(variable.name)?)
            }
            Expression::Access(Access::Property(PropertyAccess {
                object,
                property: ClassLikeMemberSelector::Identifier(property),
                ..
            })) if self.is_this(object) => self.property_type(property.value),
            Expression::Parenthesized(parenthesized) => self.receiver_type(parenthesized.expression),
            _ => None,
        }
    }

    fn visibility(&self, modifiers: &Sequence<'_, Modifier<'_>>, default: Visibility) -> Visibility {
        modifiers
            .iter()
            .find_map(|modifier| match modifier {
                Modifier::Public(_) => Some(Visibility::Public),
                Modifier::Protected(_) => Some(Visibility::Protected),
                Modifier::Private(_) => Some(Visibility::Private),
                Modifier::Internal(_) => Some(Visibility::Internal),
                _ => None,
            })
            .unwrap_or(default)
    }

    /// The visibility a type takes when it writes none: PHP#'s `internal`, spec section 5, and PHP's `public`.
    fn type_default(&self) -> Visibility {
        if self.sharp { Visibility::Internal } else { Visibility::Public }
    }

    /// The visibility a member takes when it writes none: `public` in an interface and in PHP, and PHP#'s `private`
    /// in any other type, spec section 5.
    fn member_default(&self) -> Visibility {
        if !self.sharp || self.scopes.last().is_some_and(|scope| scope.interface) {
            Visibility::Public
        } else {
            Visibility::Private
        }
    }

    /// Adds a symbol whose range runs from its first attribute, or from `span`'s start when it has none, to `span`'s
    /// end, and whose detail ends at `detail_end`, under the symbol that holds it. A member of an anonymous class is
    /// none. A function, a method and a constructor carry the complexity of `body`, their node.
    #[allow(clippy::too_many_arguments)]
    fn declare(
        &mut self,
        name: &[u8],
        kind: Kind,
        attribute_lists: Option<&Sequence<'_, AttributeList<'_>>>,
        span: Span,
        name_span: Span,
        detail_end: u32,
        visibility: Visibility,
        body: Option<Node<'_, '_>>,
    ) -> Option<usize> {
        let parent = match self.scopes.last() {
            Some(scope) => Some(scope.symbol?),
            None => None,
        };
        let attribute_lists = attribute_lists.map(Sequence::as_slice).unwrap_or_default();
        let start = attribute_lists.first().map_or(span.start, |list| list.span().start);
        let attributes = attribute_lists
            .iter()
            .flat_map(|list| list.attributes.iter())
            .map(|attribute| {
                self.class_name(&attribute.name)
                    .unwrap_or_else(|| String::from_utf8_lossy(attribute.name.value()).into_owned())
            })
            .collect();

        let symbol = Symbol {
            name: String::from_utf8_lossy(name.strip_prefix(b"$").unwrap_or(name)).into_owned(),
            kind,
            detail: self.text(start.offset, detail_end),
            range: Range { start: self.position(start.offset), end: self.position(span.end.offset) },
            selection_range: self.range(name_span),
            visibility,
            supertypes: Vec::new(),
            attributes,
            complexity: body.map(cyclomatic_complexity),
            return_type: None,
            value_type: None,
            children: Vec::new(),
        };
        self.symbols.push((parent, symbol));

        Some(self.symbols.len() - 1)
    }

    fn open(&mut self, symbol: Option<usize>, interface: bool, class: Option<Option<String>>) {
        let visibility = symbol.map_or(Visibility::Public, |index| self.symbols[index].1.visibility);

        self.scopes.push(Scope { symbol, visibility, interface, class, base: None, properties: HashMap::new() });
    }

    /// Opens the class-like `symbol`, whose supertypes are `supertypes` and whose methods read the properties `members`
    /// declares.
    fn open_class(
        &mut self,
        symbol: Option<usize>,
        interface: bool,
        supertypes: Vec<String>,
        members: &'ast [ClassLikeMember<'arena>],
    ) {
        let mut properties = HashMap::new();
        for member in members {
            match member {
                ClassLikeMember::Property(property) => {
                    let Some(hint) = property.hint() else {
                        continue;
                    };
                    for variable in property.variables() {
                        properties.insert(variable.name.strip_prefix(b"$").unwrap_or(variable.name), hint);
                    }
                }
                ClassLikeMember::Method(method) => {
                    for parameter in method.parameter_list.parameters.iter() {
                        if let (true, Some(hint)) = (parameter.is_promoted_property(), &parameter.hint) {
                            let name = parameter.variable.name;
                            properties.insert(name.strip_prefix(b"$").unwrap_or(name), hint);
                        }
                    }
                }
                _ => {}
            }
        }
        let class = symbol.map(|index| self.qualified(&self.symbols[index].1.name));
        let base = supertypes.first().cloned();
        if let Some(index) = symbol {
            self.symbols[index].1.supertypes = supertypes;
        }

        self.open(symbol, interface, Some(class));
        if let Some(scope) = self.scopes.last_mut() {
            scope.base = base;
            scope.properties = properties;
        }
    }

    fn inheritance(&self, inheritance: Option<&Inheritance<'_>>) -> Vec<String> {
        inheritance
            .into_iter()
            .flat_map(|inheritance| inheritance.types.iter())
            .filter_map(|hint| self.type_name(hint))
            .collect()
    }

    fn names_of<'a>(&self, identifiers: impl Iterator<Item = &'a Identifier<'a>>) -> Vec<String> {
        identifiers.filter_map(|identifier| self.class_name(identifier)).collect()
    }

    fn import(&mut self, prefix: Option<&[u8]>, item: &UseItem<'_>) {
        let name = item.name.value();
        let path = match prefix {
            Some(prefix) => [prefix, if self.sharp { b"." } else { b"\\" }, name].concat(),
            None => name.to_vec(),
        };

        self.imports.push(Import {
            path: String::from_utf8_lossy(path.strip_prefix(b"\\").unwrap_or(&path)).into_owned(),
            alias: item.alias.as_ref().map(|alias| String::from_utf8_lossy(alias.identifier.value).into_owned()),
            range: self.range(item.span()),
        });
    }

    fn refer(
        &mut self,
        name: &[u8],
        span: Span,
        kind: ReferenceKind,
        receiver_type: Option<String>,
        target: Option<String>,
    ) {
        self.references.push(Reference {
            offset: span.start.offset,
            name: String::from_utf8_lossy(name).into_owned(),
            kind,
            range: self.range(span),
            receiver_type,
            target,
            receiver_call: None,
            receiver_offset: None,
        });
    }

    /// Adds the call of `method` on `receiver`, whose type is `receiver_type`.
    fn call(&mut self, method: &ClassLikeMemberSelector<'_>, receiver: &Expression<'_>, receiver_type: Option<String>) {
        let ClassLikeMemberSelector::Identifier(method) = method else {
            return;
        };
        let target = receiver_type.as_deref().map(|class| self.member(class, &String::from_utf8_lossy(method.value)));

        self.refer(method.value, method.span, ReferenceKind::Call, receiver_type, target);
        if let Some(reference) = self.references.last_mut() {
            reference.receiver_offset = call_name(receiver);
        }
    }

    /// Adds each item of a field or constant line, whose detail is the line's start, then the item's name.
    fn declare_items<'item>(
        &mut self,
        items: impl Iterator<Item = (&'item [u8], Span)>,
        kind: Kind,
        attribute_lists: &Sequence<'_, AttributeList<'_>>,
        span: Span,
        visibility: Visibility,
        hint: Option<&Hint<'_>>,
    ) {
        let start = attribute_lists.first().map_or(span.start, |list| list.span().start).offset;
        let mut prefix = None;
        for (name, name_span) in items {
            let prefix = prefix.get_or_insert_with(|| self.text(start, name_span.start.offset)).clone();
            let detail = format!("{prefix} {}", String::from_utf8_lossy(name.strip_prefix(b"$").unwrap_or(name)));
            if let Some(index) =
                self.declare(name, kind, Some(attribute_lists), span, name_span, start, visibility, None)
            {
                self.symbols[index].1.detail = detail;
                self.set_type(Some(index), hint);
            }
        }
    }
}

impl<'ast, 'arena> MutWalker<'ast, 'arena, ()> for Outliner<'ast, 'arena> {
    fn walk_in_namespace(&mut self, namespace: &'ast Namespace<'arena>, _: &mut ()) {
        self.namespace = namespace.name.as_ref().map(|name| self.display(name.value()));
        if self.file_namespace.is_none() {
            self.file_namespace.clone_from(&self.namespace);
        }
    }

    fn walk_in_use(&mut self, r#use: &'ast Use<'arena>, _: &mut ()) {
        match &r#use.items {
            UseItems::Sequence(sequence) => sequence.items.iter().for_each(|item| self.import(None, item)),
            UseItems::TypedSequence(sequence) => sequence.items.iter().for_each(|item| self.import(None, item)),
            UseItems::TypedList(list) => {
                list.items.iter().for_each(|item| self.import(Some(list.namespace.value()), item));
            }
            UseItems::MixedList(list) => {
                list.items.iter().for_each(|item| self.import(Some(list.namespace.value()), &item.item));
            }
        }
    }

    fn walk_in_class(&mut self, class: &'ast Class<'arena>, _: &mut ()) {
        let visibility = self.visibility(&class.modifiers, self.type_default());
        let symbol = self.declare(
            class.name.value,
            Kind::Class,
            Some(&class.attribute_lists),
            class.span(),
            class.name.span,
            class.left_brace.start.offset,
            visibility,
            None,
        );
        self.declare_type_parameters(symbol, class.type_parameters.as_ref());
        let mut supertypes = self.inheritance(class.inheritance.as_ref());
        supertypes.extend(self.names_of(class.extends.iter().flat_map(|extends| extends.types.iter())));
        supertypes.extend(self.names_of(class.implements.iter().flat_map(|implements| implements.types.iter())));
        self.open_class(symbol, false, supertypes, class.members.as_slice());
    }

    fn walk_out_class(&mut self, _: &'ast Class<'arena>, _: &mut ()) {
        self.scopes.pop();
    }

    fn walk_in_interface(&mut self, interface: &'ast Interface<'arena>, _: &mut ()) {
        let visibility = self.visibility(&interface.modifiers, self.type_default());
        let symbol = self.declare(
            interface.name.value,
            Kind::Interface,
            Some(&interface.attribute_lists),
            interface.span(),
            interface.name.span,
            interface.left_brace.start.offset,
            visibility,
            None,
        );
        self.declare_type_parameters(symbol, interface.type_parameters.as_ref());
        let mut supertypes = self.inheritance(interface.inheritance.as_ref());
        supertypes.extend(self.names_of(interface.extends.iter().flat_map(|extends| extends.types.iter())));
        self.open_class(symbol, true, supertypes, interface.members.as_slice());
    }

    fn walk_out_interface(&mut self, _: &'ast Interface<'arena>, _: &mut ()) {
        self.scopes.pop();
    }

    fn walk_in_trait(&mut self, r#trait: &'ast Trait<'arena>, _: &mut ()) {
        let visibility = self.visibility(&r#trait.modifiers, self.type_default());
        let symbol = self.declare(
            r#trait.name.value,
            Kind::Class,
            Some(&r#trait.attribute_lists),
            r#trait.span(),
            r#trait.name.span,
            r#trait.left_brace.start.offset,
            visibility,
            None,
        );
        self.open_class(symbol, false, Vec::new(), r#trait.members.as_slice());
    }

    fn walk_out_trait(&mut self, _: &'ast Trait<'arena>, _: &mut ()) {
        self.scopes.pop();
    }

    fn walk_in_enum(&mut self, r#enum: &'ast Enum<'arena>, _: &mut ()) {
        let visibility = self.visibility(&r#enum.modifiers, self.type_default());
        let symbol = self.declare(
            r#enum.name.value,
            Kind::Enum,
            Some(&r#enum.attribute_lists),
            r#enum.span(),
            r#enum.name.span,
            r#enum.left_brace.start.offset,
            visibility,
            None,
        );
        self.declare_type_parameters(symbol, r#enum.type_parameters.as_ref());
        let mut supertypes = self.inheritance(r#enum.inheritance.as_ref());
        supertypes.extend(self.names_of(r#enum.implements.iter().flat_map(|implements| implements.types.iter())));
        self.open_class(symbol, false, supertypes, r#enum.members.as_slice());
    }

    fn walk_out_enum(&mut self, _: &'ast Enum<'arena>, _: &mut ()) {
        self.scopes.pop();
    }

    fn walk_in_struct(&mut self, r#struct: &'ast Struct<'arena>, _: &mut ()) {
        let visibility = self.visibility(&r#struct.modifiers, self.type_default());
        let symbol = self.declare(
            r#struct.name.value,
            Kind::Struct,
            Some(&r#struct.attribute_lists),
            r#struct.span(),
            r#struct.name.span,
            r#struct.left_brace.start.offset,
            visibility,
            None,
        );
        self.declare_type_parameters(symbol, r#struct.type_parameters.as_ref());
        let supertypes = self.inheritance(r#struct.inheritance.as_ref());
        self.open_class(symbol, false, supertypes, r#struct.members.as_slice());
    }

    fn walk_out_struct(&mut self, _: &'ast Struct<'arena>, _: &mut ()) {
        self.scopes.pop();
    }

    fn walk_in_anonymous_class(&mut self, class: &'ast AnonymousClass<'arena>, _: &mut ()) {
        self.open_class(None, false, Vec::new(), class.members.as_slice());
    }

    fn walk_out_anonymous_class(&mut self, _: &'ast AnonymousClass<'arena>, _: &mut ()) {
        self.scopes.pop();
    }

    /// A PHP# event runs as a class, spec section 15.
    fn walk_in_event(&mut self, event: &'ast Event<'arena>, _: &mut ()) {
        let visibility = self.visibility(&event.modifiers, self.type_default());
        let symbol = self.declare(
            event.name.value,
            Kind::Class,
            Some(&event.attribute_lists),
            event.span(),
            event.name.span,
            event.terminator.span().start.offset,
            visibility,
            None,
        );
        if let Some(index) = symbol {
            self.symbols[index].1.supertypes = self.inheritance(event.inheritance.as_ref());
        }
        self.declare_type_parameters(symbol, event.type_parameters.as_ref());
    }

    /// A PHP# module has no name of its own, as its folder's namespace names it, spec section 32.
    fn walk_in_module(&mut self, module: &'ast Module<'arena>, _: &mut ()) {
        let name = self.namespace.clone().unwrap_or_default();
        let symbol = self.declare(
            name.as_bytes(),
            Kind::Namespace,
            None,
            module.span(),
            module.module.span,
            module.left_brace.start.offset,
            self.type_default(),
            None,
        );
        self.open(symbol, false, None);
    }

    fn walk_out_module(&mut self, _: &'ast Module<'arena>, _: &mut ()) {
        self.scopes.pop();
    }

    fn walk_in_method(&mut self, method: &'ast Method<'arena>, _: &mut ()) {
        let kind = match php_method_name(method) {
            b"#statics" => Kind::Constructor,
            name if name.eq_ignore_ascii_case(b"__construct") => Kind::Constructor,
            _ => Kind::Method,
        };
        let visibility = self.visibility(&method.modifiers, self.member_default());
        let symbol = self.declare(
            method.name.value,
            kind,
            Some(&method.attribute_lists),
            method.span(),
            method.name.span,
            method.body.span().start.offset,
            visibility,
            Some(Node::Method(method)),
        );
        self.declare_type_parameters(symbol, method.type_parameters);
        match kind {
            Kind::Constructor => {
                let class = self.this();
                if let Some(index) = symbol {
                    self.symbols[index].1.return_type = Some(class);
                }
            }
            _ => self.set_return_type(symbol, method.return_type_hint.as_ref()),
        }
        self.parameters.push(HashMap::new());
    }

    fn walk_out_method(&mut self, _: &'ast Method<'arena>, _: &mut ()) {
        self.parameters.pop();
    }

    fn walk_in_named_constructor(&mut self, constructor: &'ast NamedConstructor<'arena>, _: &mut ()) {
        let visibility = self.visibility(&constructor.modifiers, self.member_default());
        let symbol = self.declare(
            constructor.name.value,
            Kind::Constructor,
            Some(&constructor.attribute_lists),
            constructor.span(),
            constructor.name.span,
            constructor.body.span().start.offset,
            visibility,
            Some(Node::NamedConstructor(constructor)),
        );
        let class = self.this();
        if let Some(index) = symbol {
            self.symbols[index].1.return_type = Some(class);
        }
    }

    fn walk_in_operator(&mut self, operator: &'ast Operator<'arena>, _: &mut ()) {
        let visibility = self.visibility(&operator.modifiers, self.member_default());
        let symbol = operator.symbol.span();
        let name = format!("operator {}", self.text(symbol.start.offset, symbol.end.offset));
        let declared = self.declare(
            name.as_bytes(),
            Kind::Method,
            Some(&operator.attribute_lists),
            operator.span(),
            Span::between(operator.operator.span, symbol),
            operator.body.span().start.offset,
            visibility,
            Some(Node::Operator(operator)),
        );
        self.set_return_type(declared, Some(&operator.return_type_hint));
    }

    /// A PHP# indexer is a property that takes parameters, spec section 6, which `this` names.
    fn walk_in_indexer(&mut self, indexer: &'ast Indexer<'arena>, _: &mut ()) {
        let visibility = self.visibility(&indexer.modifiers, self.member_default());
        let symbol = self.declare(
            indexer.this.value,
            Kind::Property,
            Some(&indexer.attribute_lists),
            indexer.span(),
            indexer.this.span,
            indexer.left_brace.start.offset,
            visibility,
            None,
        );
        self.set_type(symbol, Some(&indexer.hint));
    }

    fn walk_in_law(&mut self, law: &'ast Law<'arena>, _: &mut ()) {
        let symbol = self.declare(
            law.name.value,
            Kind::Method,
            None,
            law.span(),
            law.name.span,
            law.arrow.start.offset,
            self.member_default(),
            Some(Node::Law(law)),
        );
        self.set_return_type(symbol, None);
    }

    fn walk_in_plain_property(&mut self, property: &'ast PlainProperty<'arena>, _: &mut ()) {
        let visibility = self.visibility(&property.modifiers, self.member_default());
        let items = property.items.iter().map(|item| (item.variable().name, item.variable().span));

        self.declare_items(
            items,
            Kind::Field,
            &property.attribute_lists,
            property.span(),
            visibility,
            property.hint.as_ref(),
        );
    }

    fn walk_in_hooked_property(&mut self, property: &'ast HookedProperty<'arena>, _: &mut ()) {
        let visibility = self.visibility(&property.modifiers, self.member_default());
        let variable = property.item.variable();
        let symbol = self.declare(
            variable.name,
            Kind::Property,
            Some(&property.attribute_lists),
            property.span(),
            variable.span,
            variable.span.end.offset,
            visibility,
            None,
        );
        self.set_type(symbol, property.hint.as_ref());
    }

    fn walk_in_computed_property(&mut self, property: &'ast ComputedProperty<'arena>, _: &mut ()) {
        let visibility = self.visibility(&property.modifiers, self.member_default());
        let symbol = self.declare(
            property.variable.name,
            Kind::Property,
            Some(&property.attribute_lists),
            property.span(),
            property.variable.span,
            property.variable.span.end.offset,
            visibility,
            None,
        );
        self.set_type(symbol, property.hint.as_ref());
    }

    fn walk_in_class_like_constant(&mut self, constant: &'ast ClassLikeConstant<'arena>, _: &mut ()) {
        let visibility = self.visibility(&constant.modifiers, self.member_default());
        let items = constant.items.iter().map(|item| (item.name.value, item.name.span));

        self.declare_items(
            items,
            Kind::Constant,
            &constant.attribute_lists,
            constant.span(),
            visibility,
            constant.hint.as_ref(),
        );
    }

    fn walk_in_constant(&mut self, constant: &'ast Constant<'arena>, _: &mut ()) {
        let items = constant.items.iter().map(|item| (item.name.value, item.name.span));

        let visibility = self.type_default();
        self.declare_items(items, Kind::Constant, &constant.attribute_lists, constant.span(), visibility, None);
    }

    /// A case takes no access modifier, so it reaches as far as its enum.
    fn walk_in_enum_case(&mut self, case: &'ast EnumCase<'arena>, _: &mut ()) {
        let visibility = self.scopes.last().map_or(Visibility::Public, |scope| scope.visibility);
        let (name, detail_end) = match &case.item {
            EnumCaseItem::Unit(item) => (&item.name, item.name.span.end),
            EnumCaseItem::Backed(item) => (&item.name, item.name.span.end),
            EnumCaseItem::Data(item) => (&item.name, item.parameter_list.right_parenthesis.end),
        };
        let symbol = self.declare(
            name.value,
            Kind::EnumMember,
            Some(&case.attribute_lists),
            case.span(),
            name.span,
            detail_end.offset,
            visibility,
            None,
        );
        if let EnumCaseBody::Members(_) = case.body {
            self.open(symbol, false, None);
        }
    }

    fn walk_out_enum_case(&mut self, case: &'ast EnumCase<'arena>, _: &mut ()) {
        if let EnumCaseBody::Members(_) = case.body {
            self.scopes.pop();
        }
    }

    fn walk_in_function(&mut self, function: &'ast Function<'arena>, _: &mut ()) {
        let symbol = self.declare(
            function.name.value,
            Kind::Function,
            Some(&function.attribute_lists),
            function.span(),
            function.name.span,
            function.body.left_brace.start.offset,
            self.type_default(),
            Some(Node::Function(function)),
        );
        self.set_return_type(symbol, function.return_type_hint.as_ref());
        self.parameters.push(HashMap::new());
    }

    fn walk_out_function(&mut self, _: &'ast Function<'arena>, _: &mut ()) {
        self.parameters.pop();
    }

    fn walk_in_closure(&mut self, _: &'ast Closure<'arena>, _: &mut ()) {
        self.parameters.push(HashMap::new());
    }

    fn walk_out_closure(&mut self, _: &'ast Closure<'arena>, _: &mut ()) {
        self.parameters.pop();
    }

    fn walk_in_arrow_function(&mut self, _: &'ast ArrowFunction<'arena>, _: &mut ()) {
        self.parameters.push(HashMap::new());
    }

    fn walk_out_arrow_function(&mut self, _: &'ast ArrowFunction<'arena>, _: &mut ()) {
        self.parameters.pop();
    }

    /// A parameter types its local, and a promoted one declares a property of its class.
    fn walk_in_function_like_parameter(&mut self, parameter: &'ast FunctionLikeParameter<'arena>, _: &mut ()) {
        if let Some(hint) = &parameter.hint {
            self.locals.insert(parameter.variable.span.start.offset, hint);
            if let Some(parameters) = self.parameters.last_mut() {
                parameters.insert(parameter.variable.name, hint);
            }
        }
        if parameter.is_promoted_property() {
            let visibility = self.visibility(&parameter.modifiers, self.member_default());
            let symbol = self.declare(
                parameter.variable.name,
                Kind::Property,
                Some(&parameter.attribute_lists),
                parameter.span(),
                parameter.variable.span,
                parameter.variable.span.end.offset,
                visibility,
                None,
            );
            self.set_type(symbol, parameter.hint.as_ref());
        }
    }

    fn walk_in_local_declaration(&mut self, declaration: &'ast LocalDeclaration<'arena>, _: &mut ()) {
        if let (Some(hint), LocalTarget::Name(name)) = (declaration.hint, &declaration.target) {
            self.locals.insert(name.span.start.offset, hint);
        }
    }

    fn walk_in_function_call(&mut self, call: &'ast FunctionCall<'arena>, _: &mut ()) {
        let Expression::Identifier(function) = call.function else {
            return;
        };
        let target = match self.names.binding(function) {
            Some(Binding::Local(_)) => None,
            _ => self.names.resolve(function).map(|name| self.display(name)),
        };

        self.refer(function.value(), function.span(), ReferenceKind::Call, None, target);
    }

    fn walk_in_method_call(&mut self, call: &'ast MethodCall<'arena>, _: &mut ()) {
        let receiver_type = match self.names.static_call_class(call) {
            Some(class) => self.class_name(&class.name),
            None => self.receiver_type(call.object),
        };
        self.call(&call.method, call.object, receiver_type);
    }

    fn walk_in_null_safe_method_call(&mut self, call: &'ast NullSafeMethodCall<'arena>, _: &mut ()) {
        let receiver_type = self.receiver_type(call.object);
        self.call(&call.method, call.object, receiver_type);
    }

    fn walk_in_static_method_call(&mut self, call: &'ast StaticMethodCall<'arena>, _: &mut ()) {
        let class = match call.class {
            Expression::Identifier(class) => self.class_name(class),
            Expression::Self_(_) | Expression::Static(_) => self.this(),
            Expression::Parent(_) => self.class_scope().and_then(|scope| scope.base.clone()),
            _ => None,
        };
        self.call(&call.method, call.class, class);
    }

    fn walk_in_instantiation(&mut self, instantiation: &'ast Instantiation<'arena>, _: &mut ()) {
        if let Expression::Identifier(class) = instantiation.class {
            let target = self.class_name(class);
            self.refer(class.value(), class.span(), ReferenceKind::New, None, target);
        }
    }

    fn walk_in_class_constant_access(&mut self, access: &'ast ClassConstantAccess<'arena>, _: &mut ()) {
        let ClassLikeConstantSelector::Identifier(constant) = &access.constant else {
            return;
        };
        if !constant.value.eq_ignore_ascii_case(b"class") {
            return;
        }
        match access.class {
            Expression::Identifier(class) => {
                let target = self.class_name(class);
                self.refer(class.value(), class.span(), ReferenceKind::Type, None, target);
            }
            Expression::Self_(keyword) | Expression::Static(keyword) => {
                let target = self.this();
                self.refer(keyword.value, keyword.span, ReferenceKind::Type, None, target);
            }
            _ => {}
        }
    }

    fn walk_in_type_of(&mut self, type_of: &'ast TypeOf<'arena>, _: &mut ()) {
        let (name, span) = match &type_of.r#type {
            Hint::Identifier(identifier) => (identifier.value(), identifier.span()),
            Hint::Generic(generic) => (generic.name.value, generic.name.span),
            _ => return,
        };
        let target = match self.names.binding(&span) {
            Some(Binding::Local(_)) => None,
            _ => self.type_name(&type_of.r#type),
        };

        self.refer(name, span, ReferenceKind::Type, None, target);
    }
}
