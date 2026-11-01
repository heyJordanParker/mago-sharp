use foldhash::HashMap;
use foldhash::fast::RandomState;
use indexmap::IndexMap;

use mago_word::Word;

use crate::metadata::ttype::TypeMetadata;
use crate::misc::GenericParent;
use crate::ttype::TType;
use crate::ttype::TypeRef;
use crate::ttype::atomic::TAtomic;
use crate::ttype::atomic::reference::TReference;
use crate::ttype::union::TUnion;

pub mod bounds;
pub mod definition_type_replacer;
pub mod inferred_type_replacer;
pub mod variance;

/// Represents a template parameter definition with its source and constraint type.
///
/// This struct pairs a `GenericParent` (identifying where the template is defined)
/// with a `TUnion` (the constraint type for the template parameter).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct GenericTemplate {
    /// The entity (class or function) where this template parameter is defined.
    pub defining_entity: GenericParent,
    /// The constraint type for this template parameter (e.g., `object` for `@template T of object`).
    pub constraint: TUnion,
    /// The default type used when no explicit generic argument is provided
    /// (e.g., `string` for `@template T of int|string = string`).
    pub default: Option<TUnion>,
    /// Whether a refused PHP# bound of the same list names this type parameter, as [`refuse_bounds`] decides.
    ///
    /// `TOther` in `<T : List<TOther>, TOther>` is one. The refused bound is the refused type, so it no longer fixes or
    /// uses the type parameter, and a call reports no type argument of it unfixed, nor the type parameter unused.
    pub named_by_refused_bound: bool,
    /// The bound a PHP# type parameter writes, as the scanner read it and population resolves it. [`refuse_bounds`]
    /// derives `constraint` and `named_by_refused_bound` from it on every scan and population.
    pub written_bound: Option<TypeMetadata>,
    /// Whether the semantics checker refuses `written_bound`, as `ResolvedNames::is_refused_bound` decides.
    pub bound_refused: bool,
}

#[derive(Clone, Debug, Default)]
pub struct TemplateResult {
    pub template_types: IndexMap<Word, Vec<GenericTemplate>, RandomState>,
    pub lower_bounds: HashMap<Word, HashMap<GenericParent, Vec<TemplateBound>>>,
    pub projections: HashMap<Word, crate::ttype::template::variance::Variance>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TemplateBound {
    pub bound_type: TUnion,
    pub appearance_depth: usize,
    pub argument_offset: Option<usize>,
}

impl GenericTemplate {
    /// Creates a new `GenericTemplate` with the given source and constraint type.
    #[must_use]
    pub fn new(template_source: GenericParent, template_type: TUnion) -> Self {
        Self {
            defining_entity: template_source,
            constraint: template_type,
            default: None,
            named_by_refused_bound: false,
            written_bound: None,
            bound_refused: false,
        }
    }

    /// Returns the same `GenericTemplate` with the given default type set.
    #[must_use]
    pub fn with_default(mut self, default: Option<TUnion>) -> Self {
        self.default = default;
        self
    }
}

/// Derives the `constraint` and `named_by_refused_bound` of each template of one PHP# type parameter list from its
/// written bound, spec section 11.
///
/// A bound is refused when the checker refuses it, or when it names a type `declares` says no file declares, as
/// `T : Missing` does. A refused bound bounds nothing, so it is the refused type, and each other type parameter of the
/// list it names is named by a refused bound, so the bound's one error is the checker's or the missing type's. Any other
/// bound is the bound as written, so a population after `Missing` exists gives the real bound back. A template without
/// a written bound keeps its constraint.
pub fn refuse_bounds<'templates>(
    templates: impl IntoIterator<Item = (&'templates Word, &'templates mut GenericTemplate)>,
    declares: impl Fn(Word) -> bool,
) {
    let mut templates: Vec<(&Word, &mut GenericTemplate)> = templates.into_iter().collect();
    let mut named_by_refused_bounds = Vec::new();
    let refused: Vec<bool> = templates
        .iter()
        .map(|(name, template)| {
            let Some(bound) = &template.written_bound else {
                return false;
            };
            let mut names_missing_type = false;
            let mut named = Vec::new();
            for_each_written_part(&bound.type_union, &mut |atomic| match atomic {
                TAtomic::Reference(TReference::Symbol { name, .. }) => names_missing_type |= !declares(*name),
                TAtomic::GenericParameter(parameter)
                    if parameter.defining_entity == template.defining_entity && parameter.parameter_name != **name =>
                {
                    named.push(parameter.parameter_name);
                }
                _ => {}
            });
            let is_refused = template.bound_refused || names_missing_type;
            if is_refused {
                named_by_refused_bounds.extend(named);
            }

            is_refused
        })
        .collect();

    for ((name, template), refused) in templates.iter_mut().zip(refused) {
        if let Some(bound) = &template.written_bound {
            template.constraint = if refused { TUnion::from_atomic(TAtomic::Error) } else { bound.type_union.clone() };
        }
        template.named_by_refused_bound = named_by_refused_bounds.contains(*name);
    }
}

/// Calls `visit` on each atomic type `union` writes, at any depth, but none a type parameter's own bound holds: the
/// bound of `T` is written where `T` is declared.
pub(crate) fn for_each_written_part(union: &TUnion, visit: &mut impl FnMut(&TAtomic)) {
    fn walk(node: TypeRef<'_>, visit: &mut impl FnMut(&TAtomic)) {
        if let TypeRef::Atomic(atomic) = node {
            visit(atomic);
            if let TAtomic::GenericParameter(parameter) = atomic {
                for intersection_type in parameter.intersection_types.iter().flatten() {
                    walk(TypeRef::Atomic(intersection_type), visit);
                }

                return;
            }
        }
        for child in node.get_child_nodes() {
            walk(child, visit);
        }
    }

    walk(TypeRef::Union(union), visit);
}

impl TemplateResult {
    #[must_use]
    pub fn new(
        template_types: IndexMap<Word, Vec<GenericTemplate>, RandomState>,
        lower_bounds: HashMap<Word, HashMap<GenericParent, TUnion>>,
    ) -> TemplateResult {
        let mut new_lower_bounds = HashMap::default();

        for (k, v) in lower_bounds {
            let mut th = HashMap::default();

            for (vk, vv) in v {
                th.insert(vk, vec![TemplateBound::new(vv, 0, None)]);
            }

            new_lower_bounds.insert(k, th);
        }

        TemplateResult { template_types, lower_bounds: new_lower_bounds, projections: HashMap::default() }
    }

    #[must_use]
    pub fn has_template_types(&self) -> bool {
        !self.template_types.is_empty()
    }

    pub fn add_lower_bounds(&mut self, lower_bounds: HashMap<Word, HashMap<GenericParent, TUnion>>) {
        for (k, v) in lower_bounds {
            let mut th = HashMap::default();

            for (vk, vv) in v {
                th.insert(vk, vec![TemplateBound::new(vv, 0, None)]);
            }

            self.lower_bounds.insert(k, th);
        }
    }

    pub fn add_lower_bound(&mut self, parameter_name: Word, generic_parent: GenericParent, bound: TUnion) {
        let entry = self.lower_bounds.entry(parameter_name).or_default();

        entry.entry(generic_parent).or_default().push(TemplateBound::new(bound, 0, None));
    }

    pub fn add_template_type(&mut self, parameter_name: Word, generic_parent: GenericParent, constraint: TUnion) {
        let entry = self.template_types.entry(parameter_name).or_default();
        entry.push(GenericTemplate::new(generic_parent, constraint));
    }

    #[must_use]
    pub fn has_lower_bound(&self, parameter_name: Word, generic_parent: &GenericParent) -> bool {
        self.lower_bounds
            .get(&parameter_name)
            .and_then(|bounds| bounds.get(generic_parent))
            .is_some_and(|bounds| !bounds.is_empty())
    }

    #[must_use]
    pub fn get_lower_bounds_for_class_like(
        &self,
        parameter_name: Word,
        classlike_name: Word,
    ) -> Option<&Vec<TemplateBound>> {
        self.lower_bounds.get(&parameter_name).and_then(|bounds| bounds.get(&GenericParent::ClassLike(classlike_name)))
    }
}

impl TemplateBound {
    #[must_use]
    pub fn new(bound_type: TUnion, appearance_depth: usize, argument_offset: Option<usize>) -> Self {
        Self { bound_type, appearance_depth, argument_offset }
    }
}
