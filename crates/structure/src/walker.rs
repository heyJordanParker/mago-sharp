use foldhash::HashSet;
use foldhash::HashSetExt;
use mago_database::file::File;
use mago_names::ResolvedNames;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Class;
use mago_syntax::cst::Constant;
use mago_syntax::cst::Enum;
use mago_syntax::cst::Function;
use mago_syntax::cst::Interface;
use mago_syntax::cst::Namespace;
use mago_syntax::cst::Trait;
use mago_syntax::cst::Use;
use mago_syntax::cst::UseItem;
use mago_syntax::cst::UseItems;
use mago_syntax::cst::UseType;
use mago_syntax::walker::MutWalker;

use crate::Declaration;
use crate::DeclarationKind;
use crate::Import;
use crate::Location;
use crate::Reference;
use crate::Structure;
use crate::SymbolKind;
use crate::lean_name;

#[derive(Debug, Clone, Copy)]
pub(crate) struct StructureWalker;

#[derive(Debug)]
pub(crate) struct StructureContext<'input, 'arena> {
    file: &'input File,
    resolved_names: &'input ResolvedNames<'arena>,
    namespace: String,
    namespaces: Vec<(Span, String)>,
    claimed: HashSet<u32>,
    declarations: Vec<Declaration>,
    imports: Vec<Import>,
}

impl<'input, 'arena> StructureContext<'input, 'arena> {
    pub(crate) fn new(file: &'input File, resolved_names: &'input ResolvedNames<'arena>) -> Self {
        Self {
            file,
            resolved_names,
            namespace: String::new(),
            namespaces: Vec::new(),
            claimed: HashSet::new(),
            declarations: Vec::new(),
            imports: Vec::new(),
        }
    }

    pub(crate) fn into_structure(self) -> Structure {
        let references = self
            .resolved_names
            .iter()
            .filter(|(start, _, _, _)| !self.claimed.contains(start))
            .map(|(start, end, name, _)| Reference {
                namespace: self.namespace_at(start),
                name: lean_name(name),
                location: Location::new(self.file, start, end),
            })
            .collect();

        Structure { declarations: self.declarations, imports: self.imports, references }
    }

    fn namespace_at(&self, offset: u32) -> String {
        self.namespaces
            .iter()
            .find(|(span, _)| span.start.offset <= offset && offset < span.end.offset)
            .map(|(_, namespace)| namespace.clone())
            .unwrap_or_default()
    }

    fn location(&self, span: Span) -> Location {
        Location::new(self.file, span.start.offset, span.end.offset)
    }

    fn declare(&mut self, name: &impl HasSpan, kind: DeclarationKind) {
        let span = name.span();
        self.claimed.insert(span.start.offset);
        self.declarations.push(Declaration {
            name: lean_name(self.resolved_names.get(&span)),
            kind,
            location: self.location(span),
        });
    }

    fn import(&mut self, item: &UseItem<'_>, kind: SymbolKind) {
        let span = item.name.span();
        self.claimed.insert(span.start.offset);
        self.imports.push(Import {
            namespace: self.namespace.clone(),
            name: lean_name(self.resolved_names.get(&span)),
            kind,
            location: self.location(span),
        });
    }
}

const fn symbol_kind(r#type: Option<&UseType<'_>>) -> SymbolKind {
    match r#type {
        None => SymbolKind::ClassLike,
        Some(UseType::Function(_)) => SymbolKind::Function,
        Some(UseType::Const(_)) => SymbolKind::Constant,
    }
}

impl<'ast, 'input, 'arena> MutWalker<'ast, 'arena, StructureContext<'input, 'arena>> for StructureWalker {
    fn walk_in_namespace(
        &mut self,
        namespace: &'ast Namespace<'arena>,
        context: &mut StructureContext<'input, 'arena>,
    ) {
        context.namespace = namespace.name.as_ref().map(|name| lean_name(name.value())).unwrap_or_default();
        context.namespaces.push((namespace.span(), context.namespace.clone()));
        if let Some(name) = &namespace.name {
            context.claimed.insert(name.span().start.offset);
        }
    }

    fn walk_out_namespace(
        &mut self,
        _namespace: &'ast Namespace<'arena>,
        context: &mut StructureContext<'input, 'arena>,
    ) {
        context.namespace.clear();
    }

    fn walk_in_use(&mut self, r#use: &'ast Use<'arena>, context: &mut StructureContext<'input, 'arena>) {
        match &r#use.items {
            UseItems::Sequence(sequence) => {
                for item in &sequence.items {
                    context.import(item, SymbolKind::ClassLike);
                }
            }
            UseItems::TypedSequence(sequence) => {
                for item in &sequence.items {
                    context.import(item, symbol_kind(Some(&sequence.r#type)));
                }
            }
            UseItems::TypedList(list) => {
                context.claimed.insert(list.namespace.span().start.offset);
                for item in &list.items {
                    context.import(item, symbol_kind(Some(&list.r#type)));
                }
            }
            UseItems::MixedList(list) => {
                context.claimed.insert(list.namespace.span().start.offset);
                for item in &list.items {
                    context.import(&item.item, symbol_kind(item.r#type.as_ref()));
                }
            }
        }
    }

    fn walk_in_class(&mut self, class: &'ast Class<'arena>, context: &mut StructureContext<'input, 'arena>) {
        context.declare(&class.name, DeclarationKind::Class);
    }

    fn walk_in_interface(
        &mut self,
        interface: &'ast Interface<'arena>,
        context: &mut StructureContext<'input, 'arena>,
    ) {
        context.declare(&interface.name, DeclarationKind::Interface);
    }

    fn walk_in_trait(&mut self, r#trait: &'ast Trait<'arena>, context: &mut StructureContext<'input, 'arena>) {
        context.declare(&r#trait.name, DeclarationKind::Trait);
    }

    fn walk_in_enum(&mut self, r#enum: &'ast Enum<'arena>, context: &mut StructureContext<'input, 'arena>) {
        context.declare(&r#enum.name, DeclarationKind::Enum);
    }

    fn walk_in_function(&mut self, function: &'ast Function<'arena>, context: &mut StructureContext<'input, 'arena>) {
        context.declare(&function.name, DeclarationKind::Function);
    }

    fn walk_in_constant(&mut self, constant: &'ast Constant<'arena>, context: &mut StructureContext<'input, 'arena>) {
        for item in &constant.items {
            context.declare(&item.name, DeclarationKind::Constant);
        }
    }
}
