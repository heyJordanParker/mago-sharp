//! Move command implementation.
//!
//! `mago move` moves a PHP# class, or a whole namespace, into another namespace. It moves each class's file into the
//! folder of the namespace, rewrites the file's `namespace` line, and rewrites every file that names a moved class: a
//! PHP# `import` line, a plain PHP `use` line, a fully qualified name and a PHPDoc type. A bare name keeps naming what
//! it named: wherever the moved tree would resolve it to another class-like, it gets an `import` or a `use` of the one
//! it named, and the move refuses before writing anything when the file can't take that import. A moved namespace
//! takes each PHP# file of its folder, its modules
//! included, and each module `imports` line and `module :` that names it is rewritten. A folder the move empties is
//! deleted. A moved
//! class's name inside a string is only printed, since a string may mean anything.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::ColorChoice;
use clap::Parser;

use mago_allocator::LocalArena;
use mago_database::DatabaseReader;
use mago_database::error::DatabaseError;
use mago_database::error::IOAction;
use mago_database::file::File;
use mago_names::resolver::ChangingMembers;
use mago_names::resolver::ExternNamespaces;
use mago_names::resolver::NameResolver;
use mago_names::resolver::NamespaceClasses;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Identifier;
use mago_syntax::cst::ModuleMember;
use mago_syntax::cst::Node;
use mago_syntax::cst::Statement;
use mago_syntax::cst::Use;
use mago_syntax::cst::UseItem;
use mago_syntax::cst::UseItems;
use mago_syntax::parser::parse_file_with_settings;

use crate::config::Configuration;
use crate::error::Error;
use crate::utils::create_orchestrator;
use crate::utils::write_by_rename;

/// Move a PHP# class, or every class of a namespace, into another namespace.
#[derive(Parser, Debug)]
#[command(
    name = "move",
    about = "Move a PHP# class, or every class of a namespace, into another namespace.",
    long_about = "Move a PHP# class, or every class of a namespace, into another namespace.\n\n\
                  Moves each class's file into the namespace's folder, rewrites its `namespace` line, and rewrites \
                  every file that names a moved class: PHP# `import` lines, plain PHP `use` lines, fully qualified \
                  names and PHPDoc types. A bare name keeps naming what it named: where the move would make it \
                  name another class, the file gets an import of the one it named, and the move refuses, changing \
                  no file, when the file already declares or imports another class of that name.\n\n\
                  A moved namespace takes every PHP# file of its folder, its `Module.sharp` included, and every \
                  module `imports` line and `module :` that names it is rewritten. A folder the move empties is \
                  deleted.\n\n\
                  A moved class's name inside a string is printed with its place and never rewritten. Run \
                  `mago analyze` after the move: it reports an import cycle the move made."
)]
pub struct MoveCommand {
    /// The full name of the class to move, as `App.Shop.Order`, or of a namespace, as `App.Shop`, to move each of
    /// its classes.
    name: String,

    /// The namespace to move into, as `App.Billing`.
    target: String,
}

/// A project file as the move reads it.
struct Source {
    file: Arc<File>,
    is_sharp: bool,
    /// The file's namespace, with `\` between its parts, and the span of its name in the `namespace` line.
    namespace: Option<(String, Span)>,
    /// The offset just after the `namespace` line, where a new import goes when no `import` or `use` line stays.
    namespace_end: Option<u32>,
    /// The full name of each class-like the file declares.
    classes: Vec<String>,
    imports: Vec<Import>,
    /// Each name outside the imports, by its span, with its full name and whether an import gave it.
    names: Vec<(Span, String, bool)>,
    /// Each namespace a module's `imports` line lists, and the module an environment or test module names after `:`.
    listed_namespaces: Vec<Span>,
    strings: Vec<Span>,
    comments: Vec<Span>,
}

/// Each moved class, by its lowercase full name, with its full name and the full name it takes, both with `\` between
/// their parts.
type Moved = BTreeMap<String, (String, String)>;

/// An `import` or `use` line.
struct Import {
    span: Span,
    /// The prefix of a group, as `App\Shop` in `use App\Shop\{Order, Customer};`.
    group: Option<Span>,
    /// Each name the line imports, by the span of its name and alias, the span of its name, and its full name.
    items: Vec<(Span, Span, String)>,
}

impl MoveCommand {
    /// Moves the classes the name names into the target namespace and rewrites every file that names them.
    pub fn execute(self, configuration: Configuration, color_choice: ColorChoice) -> Result<ExitCode, Error> {
        let orchestrator = create_orchestrator(&configuration, color_choice, false, false, false);
        let database = orchestrator.load_database(&configuration.source.workspace, false, None, None)?;
        let files: Vec<Arc<File>> = database.files().filter(|file| file.file_type.is_host()).collect();
        // A class's name never depends on how names resolve, so a first read finds every class, which the second
        // resolves each file's names against, as the analyzer scans twice.
        let scanned: Vec<Source> =
            files.iter().map(|file| read(&configuration, Arc::clone(file), &NamespaceClasses::default())).collect();
        let before = namespace_classes(&scanned, |class| class.to_owned());
        let sources: Vec<Source> = files.into_iter().map(|file| read(&configuration, file, &before)).collect();

        let name = full_name(&self.name);
        let target = full_name(&self.target);
        let moved = moved_classes(&sources, &name, &target)?;
        let moved_namespace = sources.iter().find_map(|source| {
            source
                .namespace
                .as_ref()
                .filter(|(namespace, _)| namespace.eq_ignore_ascii_case(&name))
                .map(|(namespace, _)| namespace.clone())
        });
        let renamed = moved_namespace.as_deref().map(|old| (old, target.as_str()));

        let declared: BTreeSet<String> =
            sources.iter().flat_map(|source| source.classes.iter().map(|class| class.to_ascii_lowercase())).collect();
        let after = namespace_classes(&sources, |class| {
            moved.get(&class.to_ascii_lowercase()).map_or_else(|| class.to_owned(), |(_, new)| new.clone())
        });
        let workspace = &configuration.source.workspace;
        let mut planned: Vec<(PathBuf, Option<PathBuf>, String)> = Vec::new();
        let mut mentions: Vec<String> = Vec::new();
        let mut clashes: Vec<String> = Vec::new();
        for source in &sources {
            let Some(path) = source.file.path.clone() else {
                continue;
            };
            let moves = source.classes.iter().any(|class| moved.contains_key(&class.to_ascii_lowercase()))
                || (source.is_sharp
                    && source
                        .namespace
                        .as_ref()
                        .zip(moved_namespace.as_ref())
                        .is_some_and(|((own, _), old)| own == old));
            let namespace =
                if moves { Some(target.as_str()) } else { source.namespace.as_ref().map(|(name, _)| name.as_str()) };
            let contents = String::from_utf8_lossy(&source.file.contents).into_owned();
            let unimported = apply(&contents, &edits(source, namespace, &declared, &moved, renamed, &BTreeMap::new()));
            let moved_file =
                File::ephemeral(Cow::Owned(source.file.name.to_vec()), Cow::Owned(unimported.into_bytes()));
            let rebound = read(&configuration, Arc::new(moved_file), &after);
            let needed = rebinding_imports(source, &rebound, &declared, &moved)?;
            clashes.extend(import_clashes(source, &rebound, &needed, &relative(workspace, &path)));
            let edits = edits(source, namespace, &declared, &moved, renamed, &needed);
            let rewritten = apply(&contents, &edits);
            let destination = if moves { Some(destination(&path, source, &target)?) } else { None };
            if let Some(destination) = &destination
                && destination.exists()
            {
                return Err(Error::InvalidArgument(format!(
                    "Can't move `{}` into `{}`: a file is already there.",
                    path.display(),
                    destination.display()
                )));
            }
            let shown = relative(workspace, destination.as_ref().unwrap_or(&path));
            mentions.extend(string_mentions(source, &shown, &edits, &rewritten, &moved));
            if destination.is_some() || rewritten != contents {
                planned.push((path, destination, rewritten));
            }
        }

        if !clashes.is_empty() {
            return Err(Error::InvalidArgument(format!(
                "Can't move `{}` into `{}`, so no file changed:\n{}",
                dotted(&name),
                dotted(&target),
                clashes.join("\n")
            )));
        }

        let mut left: BTreeSet<PathBuf> = BTreeSet::new();
        for (path, destination, contents) in planned {
            match destination {
                Some(destination) => {
                    write_by_rename(&destination, contents.as_bytes())?;
                    std::fs::remove_file(&path)
                        .map_err(|error| DatabaseError::io(IOAction::DeleteFile, &path, error))?;
                    println!("moved {} to {}", relative(workspace, &path), relative(workspace, &destination));
                    left.extend(path.parent().map(Path::to_path_buf));
                }
                None => {
                    write_by_rename(&path, contents.as_bytes())?;
                    println!("rewrote {}", relative(workspace, &path));
                }
            }
        }
        for folder in left.iter().rev() {
            remove_emptied(workspace, folder)?;
        }
        for mention in mentions {
            println!("{mention}");
        }

        Ok(ExitCode::SUCCESS)
    }
}

/// Reads `file`: its namespace, the classes it declares, its imports, the names its code writes, each resolved as the
/// analyzer resolves it, where a class of `namespace_classes` in the file's namespace wins over the standard library's
/// class of its name (spec section 23), its strings and its comments.
fn read(configuration: &Configuration, file: Arc<File>, namespace_classes: &NamespaceClasses) -> Source {
    let arena = LocalArena::new();
    let program = parse_file_with_settings(&arena, &file, configuration.parser.to_settings());
    let resolved = NameResolver::new(&arena).resolve_in(
        program,
        namespace_classes,
        &ChangingMembers::default(),
        &ExternNamespaces::default(),
    );
    let full_names: BTreeMap<u32, (u32, String, bool)> = resolved
        .iter()
        .map(|(start, end, name, imported)| (start, (end, String::from_utf8_lossy(name).into_owned(), imported)))
        .collect();
    let full_name_at = |span: Span| full_names.get(&span.start.offset).map(|(_, name, _)| name.clone());

    let mut source = Source {
        is_sharp: file.name.ends_with(b".sharp"),
        file: Arc::clone(&file),
        namespace: None,
        namespace_end: None,
        classes: Vec::new(),
        imports: Vec::new(),
        names: Vec::new(),
        listed_namespaces: Vec::new(),
        strings: Vec::new(),
        comments: program.trivia.iter().filter(|trivia| trivia.kind.is_comment()).map(|trivia| trivia.span).collect(),
    };

    let mut statements: Vec<&Statement> = Vec::new();
    for statement in program.statements.iter() {
        if let Statement::Namespace(namespace) = statement {
            if let Some(name) = &namespace.name {
                source.namespace = Some((String::from_utf8_lossy(name.value()).replace('.', "\\"), name.span()));
            }
            source.namespace_end = Some(namespace_line_end(namespace));
            statements.extend(namespace.statements().iter());
        } else {
            statements.push(statement);
        }
    }

    let namespace = source.namespace.as_ref().map_or(String::new(), |(name, _)| format!("{name}\\"));
    for statement in statements {
        let declared = match statement {
            Statement::Class(class) => Some(class.name.value),
            Statement::Interface(interface) => Some(interface.name.value),
            Statement::Trait(r#trait) => Some(r#trait.name.value),
            Statement::Enum(r#enum) => Some(r#enum.name.value),
            Statement::Event(event) => Some(event.name.value),
            Statement::Struct(r#struct) => Some(r#struct.name.value),
            Statement::Use(r#use) => {
                source.imports.push(import(r#use, &full_name_at));
                None
            }
            Statement::Module(module) => {
                let listed = module.members.iter().filter_map(|member| match member {
                    ModuleMember::Imports(imports) => Some(imports.namespaces.iter()),
                    ModuleMember::Law(_) | ModuleMember::Binding(_) => None,
                });
                source.listed_namespaces.extend(listed.flatten().map(HasSpan::span));
                source.listed_namespaces.extend(module.parent.iter().map(|parent| parent.name.span()));
                None
            }
            _ => None,
        };
        if let Some(declared) = declared {
            source.classes.push(format!("{namespace}{}", String::from_utf8_lossy(declared)));
        }
    }

    let skipped: BTreeSet<u32> = source
        .imports
        .iter()
        .flat_map(|import| {
            import
                .items
                .iter()
                .map(|(_, name, _)| name.start.offset)
                .chain(import.group.map(|group| group.start.offset))
        })
        .chain(source.namespace.as_ref().map(|(_, span)| span.start.offset))
        .collect();
    source.names = full_names
        .iter()
        .filter(|(start, _)| !skipped.contains(start))
        .map(|(&start, (end, name, imported))| {
            (Span::new(file.id, start.into(), (*end).into()), name.clone(), *imported)
        })
        .collect();

    collect_strings(Node::Program(program), &mut source.strings);

    source
}

/// The offset just after the `namespace` line of an implicit namespace body, or after its `{`.
fn namespace_line_end(namespace: &mago_syntax::cst::Namespace<'_>) -> u32 {
    match &namespace.body {
        mago_syntax::cst::NamespaceBody::Implicit(body) => body.terminator.span().end.offset,
        mago_syntax::cst::NamespaceBody::BraceDelimited(block) => block.left_brace.end.offset,
    }
}

/// The names an `import` or `use` line imports, each with its full name.
fn import(r#use: &Use<'_>, full_name_at: &impl Fn(Span) -> Option<String>) -> Import {
    let item = |item: &UseItem<'_>, prefix: Option<&Identifier<'_>>| {
        let written = match prefix {
            Some(prefix) => {
                format!("{}\\{}", String::from_utf8_lossy(prefix.value()), String::from_utf8_lossy(item.name.value()))
            }
            None => String::from_utf8_lossy(item.name.value()).into_owned(),
        };
        let full = full_name_at(item.name.span()).unwrap_or_else(|| full_name(&written));

        (item.span(), item.name.span(), full)
    };

    match &r#use.items {
        UseItems::Sequence(sequence) => Import {
            span: r#use.span(),
            group: None,
            items: sequence.items.iter().map(|use_item| item(use_item, None)).collect(),
        },
        UseItems::MixedList(list) => Import {
            span: r#use.span(),
            group: Some(list.namespace.span()),
            items: list.items.iter().map(|maybe_typed| item(&maybe_typed.item, Some(&list.namespace))).collect(),
        },
        // A `use function` or `use const` line imports no class.
        UseItems::TypedSequence(_) | UseItems::TypedList(_) => {
            Import { span: r#use.span(), group: None, items: Vec::new() }
        }
    }
}

/// Collects the span of each literal part of every string under `node`.
fn collect_strings(node: Node<'_, '_>, strings: &mut Vec<Span>) {
    match node {
        Node::LiteralString(string) => strings.push(string.span()),
        Node::LiteralStringPart(part) => strings.push(part.span()),
        _ => {}
    }

    for child in node.children() {
        collect_strings(child, strings);
    }
}

/// Each class `name` names, by its lowercase full name, with its full name and the one it takes in `target`: the class
/// itself, or each class of the namespace `name`. Only a PHP# class moves.
fn moved_classes(sources: &[Source], name: &str, target: &str) -> Result<Moved, Error> {
    let lowercase = name.to_ascii_lowercase();
    let mut moved = BTreeMap::new();
    for source in sources {
        for class in &source.classes {
            let names_class = class.to_ascii_lowercase() == lowercase;
            let in_namespace =
                source.namespace.as_ref().is_some_and(|(namespace, _)| namespace.to_ascii_lowercase() == lowercase);
            if !(names_class || in_namespace) {
                continue;
            }

            if !source.is_sharp {
                return Err(Error::InvalidArgument(format!(
                    "`{}` is a plain PHP class, and `mago move` moves PHP# classes.",
                    dotted(class)
                )));
            }

            let short = class.rsplit('\\').next().unwrap_or(class);
            moved.insert(class.to_ascii_lowercase(), (class.clone(), format!("{target}\\{short}")));
        }
    }

    if moved.is_empty() {
        return Err(Error::InvalidArgument(format!(
            "No PHP# class or namespace of the project is named `{}`.",
            dotted(name)
        )));
    }

    let taken: BTreeSet<String> =
        sources.iter().flat_map(|source| source.classes.iter().map(|class| class.to_ascii_lowercase())).collect();
    if let Some((_, new)) = moved.values().find(|(_, new)| taken.contains(&new.to_ascii_lowercase())) {
        return Err(Error::InvalidArgument(format!("The project already has a class named `{}`.", dotted(new))));
    }

    Ok(moved)
}

/// The class-likes of `sources` that win over the standard library's class of their name in their namespace, spec
/// section 23, each by the full name `named` gives it: its name before the move, or after it.
fn namespace_classes(sources: &[Source], named: impl Fn(&str) -> String) -> NamespaceClasses {
    NamespaceClasses::new(
        sources.iter().flat_map(|source| source.classes.iter().map(|class| (named(class), source.is_sharp))),
    )
}

/// The imports that keep each bare name of `source` naming what it named before the move, by the full name each
/// imports, with the first name that needs it: a name `rebound`, `source` with the move's other edits made and read
/// against the moved classes, resolves to another class-like than the one it named before, as it lands in the moved
/// tree. A name named a class-like when the project declares it, or when it resolved to anything else than the class of
/// its namespace it would name by its text alone, as the standard library's class through spec section 23's default
/// names. A bare name stays only where it still names the same class-like, as rust-analyzer's `find_path` writes one.
fn rebinding_imports(
    source: &Source,
    rebound: &Source,
    declared: &BTreeSet<String>,
    moved: &Moved,
) -> Result<BTreeMap<String, Span>, Error> {
    if source.names.len() != rebound.names.len() {
        return Err(Error::InvalidArgument(format!(
            "`mago move` read {} names in `{}` before the move and {} after it, so it can't tell what each names.",
            source.names.len(),
            String::from_utf8_lossy(&source.file.name),
            rebound.names.len()
        )));
    }

    let contents = source.file.contents.as_ref();
    let namespace = source.namespace.as_ref().map_or("", |(namespace, _)| namespace.as_str());
    let mut needed = BTreeMap::new();
    for ((span, before, _), (_, after, _)) in source.names.iter().zip(&rebound.names) {
        let written = String::from_utf8_lossy(&contents[span.start.offset as usize..span.end.offset as usize]);
        if written.contains(['\\', '.']) {
            continue;
        }

        let by_text = if namespace.is_empty() { written.to_string() } else { format!("{namespace}\\{written}") };
        if !declared.contains(&before.to_ascii_lowercase()) && before.eq_ignore_ascii_case(&by_text) {
            continue;
        }

        let class = moved.get(&before.to_ascii_lowercase()).map_or_else(|| before.clone(), |(_, new)| new.clone());
        if !after.eq_ignore_ascii_case(&class) {
            needed.entry(class).or_insert(*span);
        }
    }

    Ok(needed)
}

/// A line for each import of `needed` that `rebound`, `source` with the move's other edits made, can't take, spec
/// section 23: one whose name it already imports from another class-like, or names a class-like it declares, or that
/// another import of `needed` takes too. Each names the file as `shown`, the line, and the name.
fn import_clashes(source: &Source, rebound: &Source, needed: &BTreeMap<String, Span>, shown: &str) -> Vec<String> {
    let short = |class: &str| class.rsplit('\\').next().unwrap_or(class).to_owned();
    let rebound_contents = rebound.file.contents.as_ref();
    let rebound_text = |span: Span| {
        String::from_utf8_lossy(&rebound_contents[span.start.offset as usize..span.end.offset as usize]).into_owned()
    };
    let imported: Vec<(String, String)> = rebound
        .imports
        .iter()
        .flat_map(|import| import.items.iter())
        .map(|(item, name, full)| {
            let alias = rebound_text(*item)[rebound_text(*name).len()..].trim().to_owned();
            let bound = alias.strip_prefix("as").map_or_else(|| short(full), |alias| alias.trim().to_owned());
            (bound, full.clone())
        })
        .collect();

    let mut clashes = Vec::new();
    for (class, span) in needed {
        let name = short(class);
        let line = source.file.line_number(span.start.offset) + 1;
        let import = if source.is_sharp { format!("import {};", dotted(class)) } else { format!("use {class};") };
        let taken = if rebound.classes.iter().any(|declared| short(declared).eq_ignore_ascii_case(&name)) {
            Some(format!("the file declares a class `{name}`"))
        } else if imported
            .iter()
            .any(|(bound, full)| bound.eq_ignore_ascii_case(&name) && !full.eq_ignore_ascii_case(class))
            || needed.keys().any(|other| other != class && short(other).eq_ignore_ascii_case(&name))
        {
            Some(format!("the file already imports another `{name}`"))
        } else {
            None
        };
        if let Some(taken) = taken {
            clashes.push(format!(
                "{shown}:{line}: `{name}` names `{}`, and keeping it needs `{import}`, but {taken}.",
                dotted(class)
            ));
        }
    }

    clashes
}

/// The edits that keep `source` naming each class it names once the classes of `moved` move, with `namespace` its
/// namespace after the move, `declared` the lowercase full name of each class of the project, and `needed` the imports
/// that keep its bare names naming what they named, as [`rebinding_imports`] gives them.
fn edits(
    source: &Source,
    namespace: Option<&str>,
    declared: &BTreeSet<String>,
    moved: &Moved,
    renamed: Option<(&str, &str)>,
    needed: &BTreeMap<String, Span>,
) -> Vec<(u32, u32, String)> {
    let new_name = |class: &str| moved.get(&class.to_ascii_lowercase()).map(|(_, new)| new.clone());
    let in_namespace = |class: &str| class.rsplit_once('\\').map_or("", |(of, _)| of) == namespace.unwrap_or_default();
    let written = |class: &str| if source.is_sharp { dotted(class) } else { class.to_owned() };
    let contents = source.file.contents.as_ref();
    let text = |span: Span| {
        String::from_utf8_lossy(&contents[span.start.offset as usize..span.end.offset as usize]).into_owned()
    };

    let mut edits = Vec::new();
    if let (Some((old, span)), Some(namespace)) = (&source.namespace, namespace)
        && old != namespace
    {
        edits.push((span.start.offset, span.end.offset, written(namespace)));
    }

    if let Some((old, new)) = renamed {
        for span in &source.listed_namespaces {
            if full_name(&text(*span)).eq_ignore_ascii_case(old) {
                edits.push((span.start.offset, span.end.offset, dotted(new)));
            }
        }
    }

    // A new import goes after the last import line that stays, so it never lands inside a line the move deletes.
    let mut kept_end = None;
    for import in &source.imports {
        // PHP# imports a class of another namespace only, spec section 23.
        if let [(_, _, full)] = import.items.as_slice()
            && source.is_sharp
            && import.group.is_none()
            && in_namespace(&new_name(full).unwrap_or_else(|| full.clone()))
        {
            let (start, end) = whole_lines(contents, import.span);
            edits.push((start, end, String::new()));
            continue;
        }

        kept_end = Some(import.span.end.offset);
        match import.group {
            None => {
                for (_, name, full) in &import.items {
                    if let Some(class) = new_name(full) {
                        let root = if contents.get(name.start.offset as usize) == Some(&b'\\') { "\\" } else { "" };
                        edits.push((name.start.offset, name.end.offset, format!("{root}{}", written(&class))));
                    }
                }
            }
            Some(prefix) if import.items.iter().any(|(_, _, full)| new_name(full).is_some()) => {
                let kept: Vec<String> = import
                    .items
                    .iter()
                    .filter(|(_, _, full)| new_name(full).is_none())
                    .map(|(item, _, _)| text(*item))
                    .collect();
                let mut statements = Vec::new();
                if !kept.is_empty() {
                    statements.push(format!("use {}\\{{{}}};", text(prefix), kept.join(", ")));
                }
                for (item, name, full) in &import.items {
                    if let Some(class) = new_name(full) {
                        let alias = &text(*item)[text(*name).len()..];
                        statements.push(format!("use {class}{alias};"));
                    }
                }
                edits.push((import.span.start.offset, import.span.end.offset, statements.join("\n")));
            }
            Some(_) => {}
        }
    }

    for (span, full, _) in &source.names {
        if declared.contains(&full.to_ascii_lowercase())
            && text(*span).contains('\\')
            && let Some(class) = new_name(full)
        {
            edits.push((span.start.offset, span.end.offset, format!("\\{class}")));
        }
    }

    if !needed.is_empty()
        && let Some(end) = kept_end.into_iter().chain(source.namespace_end).max()
    {
        let keyword = if source.is_sharp { "import" } else { "use" };
        let first = if kept_end.is_none() { "\n" } else { "" };
        let lines: Vec<String> = needed.keys().map(|class| format!("\n{keyword} {};", written(class))).collect();
        edits.push((end, end, format!("{first}{}", lines.concat())));
    }

    // A PHPDoc type names a class as plain PHP code does, so its fully qualified name moves with it.
    if !source.is_sharp {
        for comment in &source.comments {
            let comment_text = &contents[comment.start.offset as usize..comment.end.offset as usize];
            for (old, new) in moved.values() {
                let rooted = format!("\\{old}");
                for offset in occurrences(comment_text, &rooted) {
                    let start = comment.start.offset + offset as u32;
                    edits.push((start, start + rooted.len() as u32, format!("\\{new}")));
                }
            }
        }
    }

    edits
}

/// The offset of each place `text` holds `needle` as a whole name: no letter, digit, `_` or `.` before it, and no
/// letter, digit or `_` after it.
fn occurrences(text: &[u8], needle: &str) -> Vec<usize> {
    let needle = needle.as_bytes();
    let is_name = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(position) = text[from..].windows(needle.len()).position(|window| window == needle) {
        let start = from + position;
        let end = start + needle.len();
        let before = start.checked_sub(1).and_then(|before| text.get(before)).copied();
        let after = text.get(end).copied();
        if !before.is_some_and(|byte| is_name(byte) || byte == b'.') && !after.is_some_and(is_name) {
            found.push(start);
        }
        from = end;
    }

    found
}

/// Each place a string of `source` names a class of `moved` by its full name, with `\`, `\\` or `.` between its parts,
/// in any case, which the move never rewrites, at its line and column in `rewritten`, the file at `shown` once `edits`
/// apply.
fn string_mentions(
    source: &Source,
    shown: &str,
    edits: &[(u32, u32, String)],
    rewritten: &str,
    moved: &Moved,
) -> Vec<String> {
    let contents = source.file.contents.as_ref();
    let moved_to = |offset: usize| {
        let before: usize =
            edits.iter().filter(|(_, end, _)| *end as usize <= offset).map(|(_, _, text)| text.len()).sum();
        let removed: usize = edits
            .iter()
            .filter(|(_, end, _)| *end as usize <= offset)
            .map(|(start, end, _)| (end - start) as usize)
            .sum();

        offset + before - removed
    };

    let mut mentions = Vec::new();
    for string in &source.strings {
        let text = &contents[string.start.offset as usize..string.end.offset as usize];
        let lowercase = text.to_ascii_lowercase();
        for (old, (_, new)) in moved {
            for spelling in [old.clone(), old.replace('\\', "\\\\"), old.replace('\\', ".")] {
                for offset in occurrences(&lowercase, &spelling) {
                    let place = moved_to(string.start.offset as usize + offset);
                    let line_start = rewritten[..place].rfind('\n').map_or(0, |newline| newline + 1);
                    mentions.push(format!(
                        "{shown}:{}:{}: a string names `{}`, which is now `{}`. Check it by hand.",
                        rewritten[..place].matches('\n').count() + 1,
                        place - line_start + 1,
                        String::from_utf8_lossy(&text[offset..offset + spelling.len()]),
                        dotted(new),
                    ));
                }
            }
        }
    }

    mentions
}

/// Where the file at `path`, whose folder is its namespace's, goes in `target`'s folder: up one folder for each part
/// of its namespace `target` doesn't share, and down one for each part of `target` after them, spec section 5.
fn destination(path: &Path, source: &Source, target: &str) -> Result<PathBuf, Error> {
    let namespace = source.namespace.as_ref().map_or("", |(namespace, _)| namespace.as_str());
    let parts: Vec<&str> = namespace.split('\\').filter(|part| !part.is_empty()).collect();
    let target_parts: Vec<&str> = target.split('\\').collect();
    let shared = parts.iter().zip(&target_parts).take_while(|(part, target)| part == target).count();

    let mut folder = path.parent().map(Path::to_path_buf).unwrap_or_default();
    for part in parts[shared..].iter().rev() {
        if folder.file_name().is_none_or(|name| name.to_string_lossy() != *part) {
            return Err(Error::InvalidArgument(format!(
                "`{}` sits outside the folder of its namespace `{}`, so `mago move` can't tell where its namespace's folder is.",
                path.display(),
                dotted(namespace),
            )));
        }
        folder.pop();
    }

    for part in &target_parts[shared..] {
        folder.push(part);
    }

    Ok(folder.join(path.file_name().unwrap_or_default()))
}

/// Deletes `folder` when the move left it empty, and each folder above it that its deletion empties, up to the
/// workspace `workspace`.
fn remove_emptied(workspace: &Path, folder: &Path) -> Result<(), Error> {
    let mut folder = folder;
    while folder != workspace && folder.starts_with(workspace) && folder.is_dir() {
        let mut entries =
            std::fs::read_dir(folder).map_err(|error| DatabaseError::io(IOAction::OpenFolder, folder, error))?;
        if entries.next().is_some() {
            break;
        }

        std::fs::remove_dir(folder).map_err(|error| DatabaseError::io(IOAction::DeleteFolder, folder, error))?;
        println!("removed {}", relative(workspace, folder));
        let Some(parent) = folder.parent() else {
            break;
        };
        folder = parent;
    }

    Ok(())
}

/// The lines `span` covers, through the end of its last line, and one blank line after them when one is also before
/// them, so removing them leaves no second blank line.
fn whole_lines(contents: &[u8], span: Span) -> (u32, u32) {
    let start =
        contents[..span.start.offset as usize].iter().rposition(|byte| *byte == b'\n').map_or(0, |newline| newline + 1);
    let mut end = contents[span.end.offset as usize..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(contents.len(), |newline| span.end.offset as usize + newline + 1);
    if contents.get(end) == Some(&b'\n') && start >= 2 && contents[start - 2..start] == *b"\n\n" {
        end += 1;
    }

    (start as u32, end as u32)
}

/// `contents` with each of `edits` applied, an edit a replacement of the bytes from its start to its end. No two edits
/// overlap.
fn apply(contents: &str, edits: &[(u32, u32, String)]) -> String {
    let mut ordered: Vec<&(u32, u32, String)> = edits.iter().collect();
    ordered.sort_by(|left, right| right.0.cmp(&left.0).then(right.1.cmp(&left.1)));
    let mut rewritten = contents.to_owned();
    for (start, end, text) in ordered {
        rewritten.replace_range(*start as usize..*end as usize, text);
    }

    rewritten
}

/// The full name `name` writes, with `\` between its parts, as PHP writes it, and no `\` before it.
fn full_name(name: &str) -> String {
    name.trim_start_matches('\\').replace('.', "\\")
}

/// The full name `name` as PHP# writes it, with `.` between its parts.
fn dotted(name: &str) -> String {
    name.replace('\\', ".")
}

/// `path` relative to the workspace `workspace`, as the command prints it.
fn relative(workspace: &Path, path: &Path) -> String {
    path.strip_prefix(workspace).unwrap_or(path).display().to_string()
}
