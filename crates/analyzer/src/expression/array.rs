use mago_allocator::Arena;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use foldhash::HashSet;

use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::misc::GenericParent;
use mago_codex::ttype::TType;
use mago_codex::ttype::TypeRef;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::array::key::ArrayKey;
use mago_codex::ttype::atomic::array::keyed::TKeyedArray;
use mago_codex::ttype::atomic::array::list::TList;
use mago_codex::ttype::atomic::generic::TGenericParameter;
use mago_codex::ttype::atomic::mixed::TMixed;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::atomic::scalar::string::TString;
use mago_codex::ttype::combine_union_types;
use mago_codex::ttype::combiner::CombinerOptions;
use mago_codex::ttype::combiner::combine;
use mago_codex::ttype::comparator::ComparisonResult;
use mago_codex::ttype::comparator::union_comparator;
use mago_codex::ttype::comparator::union_comparator::is_contained_by;
use mago_codex::ttype::get_array_parameters;
use mago_codex::ttype::get_arraykey;
use mago_codex::ttype::get_backing_key_type;
use mago_codex::ttype::get_empty_keyed_array;
use mago_codex::ttype::get_int;
use mago_codex::ttype::get_iterable_parameters;
use mago_codex::ttype::get_iterable_value_parameter;
use mago_codex::ttype::get_literal_int;
use mago_codex::ttype::get_literal_string;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::get_never;
use mago_codex::ttype::get_string;
use mago_codex::ttype::is_key_type;
use mago_codex::ttype::union::TUnion;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::Array;
use mago_syntax::cst::ArrayElement;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Expression;
use mago_syntax::cst::LegacyArray;
use mago_syntax::cst::PatternMatchArmBody;
use mago_syntax::cst::PropertyAccess;
use mago_syntax::cst::UnaryPrefix;
use mago_syntax::cst::UnaryPrefixOperator;
use mago_syntax::cst::VariadicArrayElement;
use mago_word::WordSet;
use mago_word::empty_word;
use mago_word::word;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::artifacts::CallTarget;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::error::AnalysisError;
use crate::resolver::method::get_declared_type;
use crate::statement::class_like::r#struct::report_class_held_by_key;
use crate::statement::function_like::collection_key_error;
use crate::statement::function_like::expect_type;
use crate::utils::expression::get_block_expression_id;
use crate::utils::misc::unwrap_expression;
use crate::utils::names::display_atomic;
use crate::utils::names::display_type;
use crate::utils::names::display_value_type;
use crate::utils::quoted_source;

/// Analyzes array literals and their elements.
///
/// Example:
///
/// ```php
/// $array = [
///    'key1' => 'value1',
///    'key2' => 'value2',
/// ];
/// ```
impl<'ast, 'arena> Analyzable<'ast, 'arena> for Array<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        analyze_array_elements(context, block_context, artifacts, self.span(), self.elements.as_slice())
    }
}

/// Analyzes a legacy array literal.
///
/// Example:
///
/// ```php
/// $array = array('key1' => 'value1', 'key2' => 'value2');
/// ```
impl<'ast, 'arena> Analyzable<'ast, 'arena> for LegacyArray<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        analyze_array_elements(context, block_context, artifacts, self.span(), self.elements.as_slice())
    }
}

/// The type a PHP# literal has at a place declared a `Set`: spec section 12 makes a list literal the `Set` of its
/// elements, which the lowering keys by each element. The literal keeps this type, so its place checks its elements. A
/// `Map` literal there is refused by [`check_sharp_literal_kind`], and keeps the `Set` of its values, so its place adds
/// no second issue, unless the place also declares a `Map`, which takes it as it is. A literal at a place whose
/// elements are declared `Set`s holds `Set` literals, at any depth, as in `List<Set<int>> sets = [[1]]` and
/// `Set<Set<int>> groups = [[1, 2]]`, so it gets the type of its elements once they are `Set`s. An empty literal is the
/// collection its place declares, as [`get_placed_type`] gives it. `None` for any other value or place. The literal
/// inside any parentheses gets the type too, since the lowering reads it there. A literal that holds elements keeps
/// its own element types, and [`record_placed_type`] records the collection its place declares, at any depth. The
/// literal runs in `function_like`, None outside a function, as a property's default runs. A literal that `value`
/// gives through `??`, `? :` or a `match` takes the type too, as [`get_given_literals_type`] says.
pub(crate) fn get_set_literal_type<A>(
    context: &Context<'_, '_, A>,
    artifacts: &mut AnalysisArtifacts,
    value: &Expression<'_>,
    declared_type: &TUnion,
    function_like: Option<&FunctionLikeMetadata>,
) -> Option<TUnion>
where
    A: Arena,
{
    if !context.dialect.is_sharp() {
        return None;
    }
    let literal = unwrap_expression(value);
    let Expression::Array(array_literal) = literal else {
        let given_type = get_given_literals_type(context, artifacts, literal, declared_type, function_like)?;
        artifacts.set_expression_type(value, given_type.clone());

        return Some(given_type);
    };

    let declares_set = declared_type.types.iter().any(|atomic| matches!(atomic, TAtomic::Array(TArray::Set(_))));
    let declares_map = declared_type.types.iter().any(|atomic| matches!(atomic, TAtomic::Array(TArray::Keyed(_))));
    let becomes_set = declares_set && !(declares_map && is_map_literal(array_literal) == Some(true));
    let literal_type =
        match get_literal_of_sets_type(context, artifacts, array_literal, declared_type, becomes_set, function_like) {
            Some(literal_type) => Some(literal_type),
            None if array_literal.elements.is_empty() => get_placed_type(value, declared_type, function_like),
            None if becomes_set => {
                let array = artifacts.get_expression_type(value)?.get_single_array()?;
                let (_, element_type) = get_array_parameters(array, context.codebase);

                Some(TUnion::from_atomic(TAtomic::Array(TArray::Set(Arc::new(element_type)))))
            }
            None => None,
        };
    let own = literal_type.as_ref().or_else(|| artifacts.get_expression_type(value));
    let tuple = own.and_then(|own| get_declared_tuple(context, own, declared_type));
    let literal_type = tuple.or(literal_type);
    if let Some(literal_type) = &literal_type {
        artifacts.set_expression_type(literal, literal_type.clone());
        artifacts.set_expression_type(value, literal_type.clone());
    }
    if !array_literal.elements.is_empty() {
        record_placed_type(context, artifacts, array_literal, value, declared_type, function_like);
    }

    literal_type
}

/// The type of `value`, a `??`, a `? :` or a `match`, once each literal it gives, either side of `??`, either branch of
/// `? :` or an arm of the `match`, at any depth, takes the type [`get_set_literal_type`] gives it at a place declared
/// `declared_type`, as [`check_sharp_literal_kind`] checks each one there: the join of what it gives, the left side of
/// `??` without its `null`. The value keeps that type. `None` where no literal it gives takes a type.
fn get_given_literals_type<A>(
    context: &Context<'_, '_, A>,
    artifacts: &mut AnalysisArtifacts,
    value: &Expression<'_>,
    declared_type: &TUnion,
    function_like: Option<&FunctionLikeMetadata>,
) -> Option<TUnion>
where
    A: Arena,
{
    let (given, coalesces): (Vec<&Expression<'_>>, bool) = match value {
        Expression::Binary(binary) if binary.operator.is_null_coalesce() => (vec![binary.lhs, binary.rhs], true),
        Expression::Conditional(conditional) => {
            (vec![conditional.then.unwrap_or(conditional.condition), conditional.r#else], false)
        }
        Expression::PatternMatch(pattern_match) => (
            pattern_match
                .arms
                .iter()
                .filter_map(|arm| match arm.body() {
                    PatternMatchArmBody::Expression(arm_value) => Some(*arm_value),
                    PatternMatchArmBody::Block(_) => None,
                })
                .collect(),
            false,
        ),
        _ => return None,
    };

    let mut typed = false;
    for given_value in &given {
        typed |= get_set_literal_type(context, artifacts, given_value, declared_type, function_like).is_some();
    }
    if !typed {
        return None;
    }

    let given_types: Vec<TAtomic> = given
        .iter()
        .enumerate()
        .filter_map(|(index, given_value)| {
            let given_type = artifacts.get_expression_type(*given_value)?;

            Some(if coalesces && index == 0 { given_type.to_non_nullable() } else { given_type.clone() })
        })
        .flat_map(|given_type| given_type.types.into_owned())
        .collect();
    let given_type = TUnion::from_vec(combine(given_types, context.codebase, context.settings.combiner_options()));
    artifacts.set_expression_type(value, given_type.clone());

    Some(given_type)
}

/// The tuples of `declared` that a list literal of the type `own` becomes, spec section 30: each one it fits, so it
/// keeps the tuple's names, as `held["a"] = [owner, "a"]` writes a `[Designable? owner, string css]` into a `Map` of
/// them. `None` where it fits none.
fn get_declared_tuple<A>(context: &Context<'_, '_, A>, own: &TUnion, declared: &TUnion) -> Option<TUnion>
where
    A: Arena,
{
    let tuples: Vec<TAtomic> = declared
        .types
        .iter()
        .filter(|atomic| matches!(atomic, TAtomic::Array(TArray::List(list)) if list.tuple_names.is_some()))
        .filter(|tuple| {
            let mut result = ComparisonResult::for_dialect(context.dialect);
            is_contained_by(
                context.codebase,
                own,
                &TUnion::from_atomic((*tuple).clone()),
                false,
                false,
                false,
                &mut result,
            )
        })
        .cloned()
        .collect();

    (!tuples.is_empty()).then(|| TUnion::from_vec(tuples))
}

/// The type of the literal `array_literal`, written where `declared_type` declares its elements, once each element that
/// is a literal has the type its place gives it, as [`get_set_literal_type`] gives it: the `Set` of the elements' types
/// when the literal `becomes_set`, a `Map` of its keys to them, or else the list of each element's type in its place,
/// which a `List` and a tuple of them take, spec section 30, or a `List` of them with a spread. A missing element,
/// refused on its own, adds no type. `None` when no element took a type from its place.
fn get_literal_of_sets_type<A>(
    context: &Context<'_, '_, A>,
    artifacts: &mut AnalysisArtifacts,
    array_literal: &Array<'_>,
    declared_type: &TUnion,
    becomes_set: bool,
    function_like: Option<&FunctionLikeMetadata>,
) -> Option<TUnion>
where
    A: Arena,
{
    let collections = declared_type.types.iter().filter_map(|atomic| match atomic {
        TAtomic::Array(collection @ TArray::Set(_)) if becomes_set => Some(collection),
        TAtomic::Array(collection @ (TArray::List(_) | TArray::Keyed(_))) if !becomes_set => Some(collection),
        _ => None,
    });
    let options = context.settings.combiner_options();
    let is_list_literal = !becomes_set && is_map_literal(array_literal) == Some(false);
    let mut placed_element = false;
    let mut has_spread = false;
    let mut element_types = Vec::with_capacity(array_literal.elements.len());
    for element in array_literal.elements.iter() {
        let element_value = match element {
            ArrayElement::Value(element) => element.value,
            ArrayElement::KeyValue(element) => element.value,
            ArrayElement::Variadic(spread) => {
                let spread = artifacts.get_expression_type(spread.value)?.get_single_array()?;
                element_types.push(get_array_parameters(spread, context.codebase).1);
                has_spread = true;
                continue;
            }
            ArrayElement::Missing(_) => continue,
        };
        let position = (is_list_literal && !has_spread).then_some(element_types.len());
        let declared_element = declared_element_type(context.codebase, options, collections.clone(), position)?;
        placed_element |=
            get_set_literal_type(context, artifacts, element_value, &declared_element, function_like).is_some();
        element_types.push(artifacts.get_expression_type(element_value)?.clone());
    }
    if !placed_element {
        return None;
    }

    if is_list_literal && !has_spread {
        let elements = element_types.into_iter().map(|element_type| (false, element_type)).enumerate().collect();

        return Some(TUnion::from_atomic(TAtomic::Array(TArray::List(TList::from_known_elements(elements)))));
    }

    let element_type = element_types.into_iter().reduce(|first, other| {
        combine_union_types(&first, &other, context.codebase, context.settings.combiner_options())
    })?;
    let collection = if becomes_set {
        TArray::Set(Arc::new(element_type))
    } else if is_map_literal(array_literal)? {
        let array = artifacts.get_expression_type(array_literal)?.get_single_array()?;
        let (key_type, _) = get_array_parameters(array, context.codebase);

        TArray::Keyed(TKeyedArray::new_with_parameters(Arc::new(key_type), Arc::new(element_type)))
    } else {
        TArray::List(TList::new(Arc::new(element_type)))
    };

    Some(TUnion::from_atomic(TAtomic::Array(collection)))
}

/// The type `collections`, the collections a place declares, give the element of a literal written there at
/// `position`, spec section 14.2: the type a tuple declares for that place, spec section 30, as `[:]` takes the `Map` of
/// `[string, Map<string, string>]`, and the element type of every other collection, joined. `None` where no
/// collection is declared.
fn declared_element_type<'collection>(
    codebase: &CodebaseMetadata,
    options: CombinerOptions,
    collections: impl Iterator<Item = &'collection TArray>,
    position: Option<usize>,
) -> Option<TUnion> {
    collections
        .map(|collection| match (collection, position) {
            (TArray::List(list), Some(position))
                if let Some((_, element)) =
                    list.known_elements.as_ref().and_then(|elements| elements.get(&position)) =>
            {
                element.clone()
            }
            (collection, _) => get_array_parameters(collection, codebase).1,
        })
        .reduce(|first, other| combine_union_types(&first, &other, codebase, options))
}

/// The collection the place declared `declared` gives a literal written there as `value`: spec section 12 has a
/// literal take the type of where it lands, so the lowering makes its `Map` or `Set` with the place's type. It is the
/// place's collection of the literal's kind, as [`get_empty_literal_type`] picks it. A type parameter of
/// `function_like`, the method the literal runs in, has the type argument its call gives it, which the engine reads
/// from the method's frame. So does one of the method a lambda `function_like` is written in, as a C# closure sees
/// its method's type arguments: the lambda's frame captures the method's. `None` where the collection names a type
/// parameter of any other function, as a call's parameter names one of the method it calls, whose type arguments the
/// code that writes the literal can't spell.
fn get_placed_type(
    value: &Expression<'_>,
    declared: &TUnion,
    function_like: Option<&FunctionLikeMetadata>,
) -> Option<TUnion> {
    let placed = get_empty_literal_type(Some(value), declared)?;
    let sees = |parameter: &TGenericParameter| {
        function_like
            .and_then(|function_like| function_like.type_resolution_context.as_ref())
            .and_then(|visible| visible.get_template_definition(parameter.parameter_name))
            .is_some_and(|templates| {
                templates.iter().any(|template| template.defining_entity == parameter.defining_entity)
            })
    };
    let names_other_function_parameter = placed.get_all_child_nodes().into_iter().any(|node| {
        matches!(node, TypeRef::Atomic(TAtomic::GenericParameter(parameter))
            if matches!(parameter.defining_entity, GenericParent::FunctionLike(_)) && !sees(parameter))
    });

    (!names_other_function_parameter).then_some(placed)
}

/// Records the collection the place declared `declared` gives the literal `array_literal` that holds elements, written
/// there as `value`, as [`get_placed_type`] gives it, once the literal's own type fits it, as the declared type of the
/// literal, which the lowering makes its `Map` or `Set` with. The literal keeps its own type, so the place checks each
/// element as it is, a lambda's effects and an effect object included.
fn record_placed_type<A>(
    context: &Context<'_, '_, A>,
    artifacts: &mut AnalysisArtifacts,
    array_literal: &Array<'_>,
    value: &Expression<'_>,
    declared: &TUnion,
    function_like: Option<&FunctionLikeMetadata>,
) where
    A: Arena,
{
    let Some(placed) = get_placed_type(value, declared, function_like) else {
        return;
    };
    let Some(own) = artifacts.get_expression_type(value) else {
        unreachable!("the analysis types a literal before its place takes it, not the one at {:?}", value.span());
    };
    let mut result = ComparisonResult::for_dialect(context.dialect);
    if is_contained_by(context.codebase, own, &placed, false, false, false, &mut result) {
        artifacts.record_declared_type(array_literal, placed);
    }
}

/// The type an empty value written to a place declared `declared` has there: PHP types `[]` and `[:]` as one empty
/// array, and the place keeps the collection it declares, so its rules and elements stay. `[]` is the place's `List`
/// or `Set` and `[:]` its `Map`, whichever its union names, and `[]` where only an `Iterable<T>` is named its
/// `List<T>`. A literal no collection there takes, which
/// [`check_sharp_literal_kind`] refuses, and any other empty value keep every collection the place declares, so the
/// place adds no second issue. `None` where `declared` holds no collection.
pub(crate) fn get_empty_literal_type(value: Option<&Expression<'_>>, declared: &TUnion) -> Option<TUnion> {
    let collections = declared.types.iter().filter(|atomic| matches!(atomic, TAtomic::Array(_)));
    let is_map_literal = match value.map(Expression::unparenthesized) {
        Some(Expression::Array(literal)) => is_map_literal(literal),
        _ => None,
    };
    let taken: Vec<TAtomic> = collections
        .clone()
        .filter(|atomic| Some(matches!(atomic, TAtomic::Array(TArray::Keyed(_)))) == is_map_literal)
        .cloned()
        .collect();
    let mut types: Vec<TAtomic> = if taken.is_empty() { collections.cloned().collect() } else { taken };
    // A `List` is an `Iterable`, spec section 12, so `[]` at a place that declares only `Iterable<T>` is a `List<T>`.
    if types.is_empty() && is_map_literal == Some(false) {
        types = declared
            .types
            .iter()
            .filter_map(|atomic| match atomic {
                TAtomic::Iterable(iterable) if iterable.get_key_type().is_mixed() => {
                    Some(TAtomic::Array(TArray::List(TList::new(Arc::clone(&iterable.value_type)))))
                }
                _ => None,
            })
            .collect();
    }

    (!types.is_empty()).then(|| TUnion::from_vec(types))
}

/// Gives `literal`, a PHP# value written as a collection literal, the collection type of `others`, the values written
/// beside it, as a place gives its literal one, spec section 12: the other side of `==`, `!=` or `??`, the other branch
/// of `? :`, or the other arms of a `match`. A list literal beside a `Set` is a `Set`, at any depth, `[]` is the `List`
/// or `Set` beside it, and `[:]` its `Map`. A literal of a kind no value beside it is, as `[]` beside a `Map`, is refused
/// as [`check_sharp_literal_kind`] refuses it at a place, and a list literal beside both a `List` and a `Set` is refused,
/// since nothing says which it is. A refused literal takes the type beside it, so nothing that reads it reports again.
/// A literal beside only literals, or beside no collection, keeps its own type.
pub(crate) fn type_collection_literal<'arena, A>(
    context: &mut Context<'_, 'arena, A>,
    block_context: &BlockContext<'_>,
    artifacts: &mut AnalysisArtifacts,
    literal: &Expression<'arena>,
    others: &[&Expression<'_>],
) where
    A: Arena,
{
    let Expression::Array(array) = literal.unparenthesized() else {
        return;
    };
    if !context.dialect.is_sharp() {
        return;
    }
    let collections: Vec<TAtomic> = others
        .iter()
        .filter(|other| !matches!(other.unparenthesized(), Expression::Array(_)))
        .filter_map(|other| artifacts.get_expression_type(*other))
        .flat_map(|other_type| other_type.types.iter().filter(|atomic| matches!(atomic, TAtomic::Array(_))).cloned())
        .collect();
    if collections.is_empty() {
        return;
    }
    let other_type = TUnion::from_vec(combine(collections, context.codebase, context.settings.combiner_options()));

    let literal_type = if check_sharp_literal_kind(context, Some(block_context), artifacts, literal, &other_type)
        || report_list_or_set_literal(context, array, &other_type)
    {
        Some(other_type)
    } else if array.elements.is_empty() {
        get_empty_literal_type(Some(literal), &other_type)
    } else {
        get_set_literal_type(context, artifacts, literal, &other_type, block_context.scope.get_function_like())
    };
    if let Some(literal_type) = literal_type {
        artifacts.set_expression_type(array, literal_type.clone());
        artifacts.set_expression_type(literal, literal_type);
    }
}

/// Reports `literal`, a list literal written beside `beside`, the values that give it its type, when they hold both a
/// `List` and a `Set`, so the literal could be either. Returns whether it reported.
fn report_list_or_set_literal<A>(context: &mut Context<'_, '_, A>, literal: &Array<'_>, beside: &TUnion) -> bool
where
    A: Arena,
{
    let collection = |is_set: bool| {
        beside.types.iter().find(|atomic| match atomic {
            TAtomic::Array(TArray::Set(_)) => is_set,
            TAtomic::Array(TArray::List(_)) => !is_set,
            _ => false,
        })
    };
    let (Some(false), Some(list), Some(set)) = (is_map_literal(literal), collection(false), collection(true)) else {
        return false;
    };

    let (list, set) = (display_atomic(context, list), display_atomic(context, set));
    let written = quoted_source(context, literal.span());
    let beside = display_type(context, beside);
    context.collector.report_with_code(
        IssueCode::InvalidArrayElement,
        Issue::error(format!(
            "`{written}` could be a `{list}` or a `{set}` here. Declare the one it is, as in `{set} values = {written};`."
        ))
        .with_annotation(Annotation::primary(literal.span()).with_message(format!("Written beside `{beside}`."))),
    );

    true
}

#[derive(Debug)]
struct ArrayCreationInfo {
    item_key_atomic_types: Vec<TAtomic>,
    item_value_atomic_types: Vec<TAtomic>,
    property_types: BTreeMap<ArrayKey, (bool, TUnion)>,
    class_strings: WordSet,
    can_create_objectlike: bool,
    array_keys: HashSet<ArrayKey>,
    int_offset: i64,
    is_list: bool,
    known_non_list: bool,
    can_be_empty: bool,
    known_int_offset: bool,
}

fn report_unpacked_int_key_overflow<A>(context: &mut Context<'_, '_, A>, element_span: Span)
where
    A: Arena,
{
    context.collector.report_with_code(
        IssueCode::InvalidArrayIndex,
        Issue::error("Cannot add an item with an offset beyond `PHP_INT_MAX`.")
            .with_annotation(Annotation::primary(element_span).with_message("Adding this item would result in an invalid integer key."))
            .with_note(format!("PHP automatically assigns integer keys starting from the highest previous integer key. The next key would exceed `PHP_INT_MAX` ({}).", i64::MAX))
            .with_note("This usually happens in very large arrays or after using an explicit integer key close to the maximum.")
            .with_help("Consider using an explicit string key for this item, restructuring the array, or ensuring previous explicit integer keys are smaller."),
    );
}

fn analyze_array_elements<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    expression_span: Span,
    elements: &[ArrayElement<'arena>],
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    if elements.is_empty() {
        artifacts.set_expression_type(&expression_span, get_empty_keyed_array());

        return Ok(());
    }

    let mut array_creation_info = ArrayCreationInfo {
        item_key_atomic_types: Vec::new(),
        item_value_atomic_types: Vec::new(),
        property_types: BTreeMap::default(),
        class_strings: WordSet::default(),
        can_create_objectlike: true,
        array_keys: HashSet::default(),
        int_offset: -1,
        is_list: true,
        known_non_list: false,
        can_be_empty: true,
        known_int_offset: true,
    };

    // Spec section 14.2: each element of a collection literal takes the element type its collection type declares, as C#
    // types a collection initializer's elements, so a lambda in it takes its parameter types from it.
    let expected = artifacts.get_expected_type(&expression_span).cloned();
    let codebase = context.codebase;
    let options = context.settings.combiner_options();
    let element_type = |position: Option<&ArrayKey>| {
        let collections =
            expected.iter().flat_map(|expected| expected.types.iter()).filter_map(|atomic| match atomic {
                TAtomic::Array(collection) => Some(collection),
                _ => None,
            });
        let position = match position {
            Some(ArrayKey::Integer(index)) => usize::try_from(*index).ok(),
            _ => None,
        };

        declared_element_type(codebase, options, collections, position)
    };

    for element in elements {
        let (item_key_value, key_type, item_is_list_item, value) = match element {
            ArrayElement::KeyValue(key_value_array_element) => {
                let was_inside_general_use = block_context.flags.inside_general_use();
                block_context.flags.set_inside_general_use(true);
                key_value_array_element.key.analyze(context, block_context, artifacts)?;
                block_context.flags.set_inside_general_use(was_inside_general_use);

                let (item_key_value, key_type) = artifacts
                    .get_expression_type(key_value_array_element.key)
                    .map_or((None, get_mixed()), |item_key_type| {
                        let key_type = if context.dialect.is_sharp() {
                            // A `Map` keeps each key as itself, so `1.5`, `true` and `null` stay keys PHP would
                            // cast. A key without `==` and `hash()` names the rule a `Map` type keeps, and its error is
                            // its only report.
                            if is_key_type(item_key_type, context.codebase) {
                                let span = key_value_array_element.key.span();
                                if report_class_held_by_key(context, item_key_type, false, span) {
                                    get_never()
                                } else {
                                    item_key_type.clone()
                                }
                            } else {
                                let item_key_type_id = display_value_type(context, item_key_type, &get_arraykey());
                                let error = collection_key_error(
                                    context.codebase,
                                    item_key_type,
                                    item_key_type_id.as_str(),
                                    false,
                                    key_value_array_element.key.span(),
                                );
                                context.collector.report_with_code(IssueCode::InvalidArrayElementKey, error);

                                get_never()
                            }
                        } else if item_key_type.is_null() {
                            get_literal_string(empty_word())
                        } else if item_key_type.is_true() {
                            get_literal_int(1)
                        } else if item_key_type.is_false() {
                            get_literal_int(0)
                        } else if let Some(f) = item_key_type.get_single_literal_float_value() {
                            get_literal_int(f.trunc() as i64)
                        } else if item_key_type.is_float() {
                            get_int()
                        } else if !item_key_type.is_always_array_key(true) {
                            let item_key_type_id = display_type(context, item_key_type);
                            let key_span = key_value_array_element.key.span();
                            context.collector.report_with_code(
                                IssueCode::InvalidArrayElementKey,
                                Issue::error("Invalid array key type.")
                                    .with_annotation(Annotation::primary(key_span).with_message(format!(
                                        "This has type `{item_key_type_id}`, which cannot be cast to a string or integer.",
                                    )))
                                    .with_note(format!(
                                        "In PHP, array keys must be strings or integers. While types like `bool` or `float` are automatically cast, a value of type `{item_key_type_id}` cannot be.",
                                    ))
                                    .with_help("Ensure the array key is either a string or an integer."),
                            );

                            get_arraykey()
                        } else {
                            item_key_type.clone()
                        };

                        let item_key_value =
                            if let Some(item_key_literal_type) = key_type.get_single_literal_string_value() {
                                // A `Map` keeps `"5"` a string, where PHP casts it to the int `5`.
                                Some(if context.dialect.is_sharp() {
                                    ArrayKey::String(word(item_key_literal_type))
                                } else {
                                    ArrayKey::from_string(word(item_key_literal_type))
                                })
                            } else if let Some(literal_integer) = key_type.get_single_literal_int_value() {
                                // The most recent integer key becomes the next available integer key
                                array_creation_info.int_offset = literal_integer;

                                Some(ArrayKey::Integer(literal_integer))
                            } else if let Some(class_string) = key_type.get_single_class_string_value() {
                                array_creation_info.class_strings.insert(class_string);

                                Some(ArrayKey::String(class_string))
                            } else {
                                None
                            };

                        (item_key_value, key_type)
                    });

                (item_key_value, key_type, false, key_value_array_element.value)
            }
            ArrayElement::Value(value_array_element) => {
                // check if we have reached PHP_INT_MAX
                if array_creation_info.int_offset == i64::MAX {
                    context.collector.report_with_code(
                        IssueCode::InvalidArrayIndex,
                        Issue::error(
                            "Cannot add array item implicitly; the next available integer key would exceed PHP_INT_MAX."
                        )
                        .with_annotation(
                            Annotation::primary(value_array_element.span())
                                .with_message("Adding this item would result in an invalid integer key.")
                        )
                        .with_note(
                            format!("PHP automatically assigns integer keys starting from the highest previous integer key. The next key would exceed `PHP_INT_MAX` ({}).", i64::MAX)
                        )
                        .with_note(
                            "This usually happens in very large arrays or after using an explicit integer key close to the maximum."
                        )
                        .with_help(
                            "Consider using an explicit string key for this item, restructuring the array, or ensuring previous explicit integer keys are smaller."
                        ),
                    );

                    break;
                }

                if array_creation_info.known_int_offset {
                    array_creation_info.int_offset += 1;

                    (
                        Some(ArrayKey::Integer(array_creation_info.int_offset)),
                        get_literal_int(array_creation_info.int_offset),
                        true,
                        value_array_element.value,
                    )
                } else {
                    (None, get_int(), true, value_array_element.value)
                }
            }
            ArrayElement::Variadic(variadic_array_element) => {
                let was_inside_general_use = block_context.flags.inside_general_use();
                block_context.flags.set_inside_general_use(true);
                variadic_array_element.value.analyze(context, block_context, artifacts)?;
                block_context.flags.set_inside_general_use(was_inside_general_use);

                if let Some(variadic_array_element_type) = artifacts.get_expression_type(&variadic_array_element.value)
                {
                    handle_variadic_array_element(
                        context,
                        &mut array_creation_info,
                        variadic_array_element,
                        variadic_array_element_type,
                    );
                } else {
                    array_creation_info.can_create_objectlike = false;
                    array_creation_info.item_key_atomic_types.push(TAtomic::Scalar(TScalar::ArrayKey));
                    array_creation_info.item_value_atomic_types.push(TAtomic::Mixed(TMixed::new()));
                }

                continue;
            }
            ArrayElement::Missing(missing_array_element) => {
                context.collector.report_with_code(
                    IssueCode::InvalidArrayElement,
                    Issue::error(
                        "Missing array element: skipping elements is only allowed in list assignments (destructuring)."
                    )
                    .with_annotation(
                        Annotation::primary(missing_array_element.span())
                            .with_message("Element expected here.")
                    )
                    .with_note(
                        "Array literals require a value for each position (e.g., `[1, null, 3]`) unless used on the left side of an assignment (e.g., `[$a, , $c] = ...;`)."
                    )
                    .with_help("Provide a value for this element (e.g., `null`) or remove the extra comma."),
                );

                continue;
            }
        };

        let was_inside_general_use = block_context.flags.inside_general_use();
        block_context.flags.set_inside_general_use(true);
        expect_type(context, artifacts, value, element_type(item_key_value.as_ref()).as_ref());
        value.analyze(context, block_context, artifacts)?;
        block_context.flags.set_inside_general_use(was_inside_general_use);

        array_creation_info.can_be_empty = false;
        array_creation_info.is_list &= item_is_list_item;

        if let Some(item_key_value) = item_key_value {
            let is_duplicate = array_creation_info.array_keys.contains(&item_key_value);
            if !is_duplicate
                && !matches!(
                    item_key_value,
                    ArrayKey::Integer(index)
                        if usize::try_from(index).is_ok_and(|index| index == array_creation_info.array_keys.len())
                )
            {
                array_creation_info.known_non_list = true;
            }

            if is_duplicate {
                context.collector.report_with_code(
                    IssueCode::DuplicateArrayKey,
                    Issue::error(format!(
                        "Duplicate array key `{item_key_value}` detected."
                    ))
                    .with_annotation(
                        Annotation::primary(element.span())
                            .with_message(format!("This key `{item_key_value}` duplicates an earlier key"))
                    )
                    .with_note(
                        "Using the same key multiple times in an array literal will overwrite the previous value associated with that key."
                    )
                    .with_help(
                        "Remove the duplicate entry or use a unique key for this element."
                    ),
                );
            } else {
                array_creation_info.array_keys.insert(item_key_value);
            }
        }

        if let Expression::UnaryPrefix(UnaryPrefix { operator: UnaryPrefixOperator::Reference(_), operand }) =
            unwrap_expression(value)
        {
            let variable_id = get_block_expression_id(operand, context, block_context);
            if let Some(variable_id) = variable_id {
                if let Some(existing_type) = block_context.locals.remove(&variable_id) {
                    block_context.remove_descendants(context, variable_id, &existing_type, None);
                }

                block_context.locals.insert(variable_id, Rc::new(get_mixed()));
            }
        }

        match artifacts.get_expression_type(value) {
            Some(value_type) => {
                if let Some(item_key_value) = item_key_value {
                    array_creation_info.property_types.insert(item_key_value, (false, value_type.clone()));
                } else {
                    array_creation_info.can_create_objectlike = false;
                    array_creation_info.item_key_atomic_types.extend(key_type.types.into_owned());
                    array_creation_info.item_value_atomic_types.extend(value_type.types.iter().cloned());
                }
            }
            None => {
                if let Some(item_key_value) = item_key_value {
                    array_creation_info.property_types.insert(item_key_value, (false, get_mixed()));
                } else {
                    array_creation_info.can_create_objectlike = false;
                    array_creation_info.item_key_atomic_types.extend(key_type.types.into_owned());
                    array_creation_info.item_value_atomic_types.push(TAtomic::Mixed(TMixed::new()));
                }
            }
        }
    }

    if array_creation_info.is_list
        && array_creation_info.property_types.len() == 2
        && let (Some((_, first_type)), Some((_, second_type))) = (
            array_creation_info.property_types.get(&ArrayKey::Integer(0)),
            array_creation_info.property_types.get(&ArrayKey::Integer(1)),
        )
    {
        let class_names: Vec<_> = first_type
            .types
            .iter()
            .filter_map(|atomic| {
                if let Some(class_name) = atomic.get_class_string_value() {
                    Some(class_name)
                } else if let Some(literal_str) = atomic.get_literal_string_value() {
                    Some(word(literal_str))
                } else if let TAtomic::Object(obj) = atomic {
                    match obj {
                        TObject::Named(named) => Some(named.name),
                        TObject::Enum(r#enum) => Some(r#enum.name),
                        _ => None,
                    }
                } else {
                    None
                }
            })
            .collect();

        let method_names: Vec<_> =
            second_type.types.iter().filter_map(|atomic| atomic.get_literal_string_value()).collect();

        for class_name in &class_names {
            for method_name in &method_names {
                if class_name.is_empty() || method_name.is_empty() {
                    continue;
                }

                // Each by its key, spec section 24: the class's in the codebase, and the method's in its class.
                let class = context.codebase.class_key(*class_name);
                artifacts.symbol_references.add_reference_to_class_member(
                    &block_context.scope,
                    (class, context.codebase.method_key(class, word(method_name))),
                    false,
                );
            }
        }
    }

    let item_key_type = if array_creation_info.item_key_atomic_types.is_empty() {
        None
    } else {
        Some(TUnion::from_vec(combine(
            array_creation_info.item_key_atomic_types,
            context.codebase,
            context.settings.combiner_options(),
        )))
    };

    let item_value_type = if array_creation_info.item_value_atomic_types.is_empty() {
        None
    } else {
        Some(TUnion::from_vec(combine(
            array_creation_info.item_value_atomic_types,
            context.codebase,
            context.settings.combiner_options(),
        )))
    };

    let array_type = if !array_creation_info.property_types.is_empty() {
        if array_creation_info.is_list {
            TUnion::from_vec(vec![TAtomic::Array(TArray::List(TList {
                known_count: if array_creation_info.known_int_offset {
                    Some(array_creation_info.property_types.len())
                } else {
                    None
                },
                known_elements: Some(
                    array_creation_info
                        .property_types
                        .into_iter()
                        .enumerate()
                        .map(|(index, (_, value_tuple))| (index, (value_tuple.0, value_tuple.1)))
                        .collect(),
                ),
                element_type: Arc::new(match item_value_type {
                    Some(value) => value,
                    None => get_never(),
                }),
                non_empty: true,
                tuple_names: None,
            }))])
        } else {
            TUnion::from_vec(vec![TAtomic::Array(TArray::Keyed(TKeyedArray {
                known_items: Some(
                    array_creation_info.property_types.into_iter().map(|(k, v)| (k, (v.0, v.1))).collect(),
                ),
                parameters: if array_creation_info.can_create_objectlike {
                    None
                } else {
                    match (item_key_type, item_value_type) {
                        (Some(key), Some(value)) => Some((Arc::new(key), Arc::new(value))),
                        (Some(key), None) => Some((Arc::new(key), Arc::new(get_mixed()))),
                        (None, Some(value)) => Some((Arc::new(get_arraykey()), Arc::new(value))),
                        _ => Some((Arc::new(get_arraykey()), Arc::new(get_mixed()))),
                    }
                },
                non_empty: true,
                known_non_list: array_creation_info.known_non_list,
            }))])
        }
    } else if item_key_type.is_none() && item_value_type.is_none() {
        get_empty_keyed_array()
    } else if array_creation_info.is_list {
        TUnion::from_vec(vec![TAtomic::Array(TArray::List(TList {
            known_elements: None,
            element_type: Arc::new(item_value_type.unwrap_or_else(get_mixed)),
            known_count: None,
            non_empty: !array_creation_info.can_be_empty,
            tuple_names: None,
        }))])
    } else {
        TUnion::from_vec(vec![TAtomic::Array(TArray::Keyed(TKeyedArray {
            known_items: None,
            parameters: match (item_key_type, item_value_type) {
                (Some(key), Some(value)) => Some((Arc::new(key), Arc::new(value))),
                (Some(key), None) => Some((Arc::new(key), Arc::new(get_mixed()))),
                (None, Some(value)) => Some((Arc::new(get_arraykey()), Arc::new(value))),
                _ => Some((Arc::new(get_arraykey()), Arc::new(get_mixed()))),
            },
            non_empty: !array_creation_info.can_be_empty,
            known_non_list: array_creation_info.known_non_list,
        }))])
    };

    if context.dialect.is_sharp() {
        report_sharp_literal_spreads(context, artifacts, elements);
    }

    artifacts.set_expression_type(&expression_span, array_type);

    Ok(())
}

/// Reports a PHP# literal whose spreads do not make one collection. A `List` spread appends, so `List` values and
/// spreads make a `List`. A `Map` spread keeps its keys, so `Map` entries and spreads make a `Map`. A literal never
/// holds a `List`'s values and a `Map`'s entries together.
fn report_sharp_literal_spreads<A>(
    context: &mut Context<'_, '_, A>,
    artifacts: &AnalysisArtifacts,
    elements: &[ArrayElement<'_>],
) where
    A: Arena,
{
    if !elements.iter().any(|element| matches!(element, ArrayElement::Variadic(_))) {
        return;
    }

    let mut list_part: Option<Span> = None;
    let mut map_part: Option<Span> = None;
    for element in elements {
        let is_map = match element {
            ArrayElement::Value(_) => false,
            ArrayElement::KeyValue(_) => true,
            ArrayElement::Variadic(spread) => {
                let Some(spread_type) = artifacts.get_expression_type(&spread.value) else {
                    continue;
                };

                let is_list_or_map =
                    |atomic: &TAtomic| matches!(atomic, TAtomic::Array(TArray::List(_) | TArray::Keyed(_)));

                if spread_type.types.iter().all(|atomic| atomic.is_list() || atomic.is_never()) {
                    false
                } else if spread_type.types.iter().all(|atomic| is_list_or_map(atomic) || atomic.is_never()) {
                    true
                } else {
                    // A value that may not be iterable already has PHP's own error.
                    if let Some(atomic) = spread_type.types.iter().find(|atomic| {
                        !is_list_or_map(atomic) && get_iterable_value_parameter(atomic, context.codebase).is_some()
                    }) {
                        let type_str = display_atomic(context, atomic);
                        context.collector.report_with_code(
                            IssueCode::InvalidArrayElement,
                            Issue::error(format!(
                                "Cannot spread a value of type `{type_str}`: PHP# spreads a `List` or a `Map`."
                            ))
                            .with_annotation(
                                Annotation::primary(element.span())
                                    .with_message(format!("Type `{type_str}` is neither a `List` nor a `Map`")),
                            )
                            .with_help("Spread a `List`, such as a `list<int>` from plain PHP."),
                        );
                    }

                    continue;
                }
            }
            ArrayElement::Missing(_) => continue,
        };

        let (own_part, other_part) = if is_map { (&mut map_part, list_part) } else { (&mut list_part, map_part) };
        if let Some(other_part) = other_part {
            let (part, other) = if is_map { ("`Map`", "`List`") } else { ("`List`", "`Map`") };
            context.collector.report_with_code(
                IssueCode::InvalidArrayElement,
                Issue::error("A literal cannot hold a `List`'s values and a `Map`'s entries together.")
                    .with_annotation(Annotation::primary(element.span()).with_message(format!("A {part}'s part.")))
                    .with_annotation(Annotation::secondary(other_part).with_message(format!("A {other}'s part.")))
                    .with_help("Build the `List` and the `Map` in literals of their own."),
            );

            return;
        }

        own_part.get_or_insert(element.span());
    }
}

/// Reports a PHP# literal written where the other collection is declared. `[]` and `[a, b]` are `List` literals, and
/// `[:]` and `[key: value]` are `Map` literals, whatever their place declares. A place declared a `Set` takes a `List`
/// literal, which becomes the `Set` of its elements. A literal is refused only when no collection in `declared` takes
/// its kind, whatever their order, so `List.wrap`'s `T|List<T>` takes either literal once `T` is a `Map`. A literal
/// reaches the place as `value` itself, or through the right side of `??`, either branch of `? :` or an arm of a
/// `match`, at any depth. A literal of spreads names no collection of its own, and `report_sharp_literal_spreads`
/// checks what it spreads.
///
/// A `List` value that is no literal is refused the same way where only a `Map` is declared, once `artifacts` types
/// it: its keys are only its positions. A local, a parameter or a property counts by its declared type, which
/// `block_context` holds, so one declared a `Map` that a refused literal set is not refused again. Plain PHP's value is
/// the exception, since it still holds the keys plain PHP gave it, as [`is_plain_php_value`] says, and the place makes
/// a `Map` of them. Returns whether it refused a literal.
pub(crate) fn check_sharp_literal_kind<'arena, A>(
    context: &mut Context<'_, 'arena, A>,
    block_context: Option<&BlockContext<'_>>,
    artifacts: &AnalysisArtifacts,
    value: &Expression<'arena>,
    declared: &TUnion,
) -> bool
where
    A: Arena,
{
    if !context.dialect.is_sharp() {
        return false;
    }

    let collections = declared.types.iter().filter_map(|atomic| match atomic {
        TAtomic::Array(array) => Some(array),
        _ => None,
    });
    let Some(collection) = collections.clone().next() else {
        return false;
    };
    let mut refused = false;
    let takes_map_literal = collections.clone().any(|array| matches!(array, TArray::Keyed(_)));
    let takes_list_literal = collections.clone().any(|array| matches!(array, TArray::List(_) | TArray::Set(_)));

    // The values still to look at, last first, so the literals are reported in the order they are written.
    let mut values = vec![value];
    while let Some(value) = values.pop() {
        let literal = match value.unparenthesized() {
            Expression::Array(literal) => literal,
            Expression::Binary(binary) if binary.operator.is_null_coalesce() => {
                values.push(binary.rhs);
                values.push(binary.lhs);
                continue;
            }
            Expression::Conditional(conditional) => {
                values.push(conditional.r#else);
                values.push(conditional.then.unwrap_or(conditional.condition));
                continue;
            }
            Expression::PatternMatch(pattern_match) => {
                values.extend(pattern_match.arms.iter().rev().filter_map(|arm| match arm.body() {
                    PatternMatchArmBody::Expression(arm_value) => Some(*arm_value),
                    PatternMatchArmBody::Block(_) => None,
                }));
                continue;
            }
            value => {
                if takes_map_literal && !takes_list_literal {
                    report_list_in_map_place(context, block_context, artifacts, value, declared);
                }
                continue;
            }
        };

        let Some(is_map_literal) = is_map_literal(literal) else {
            continue;
        };
        if (is_map_literal && takes_map_literal) || (!is_map_literal && takes_list_literal) {
            // Each element lands in the value type of the collections that take the literal, so a literal it holds is
            // checked there, as `["x": ['a', 'b']]` in a `Map<string, Map<int, string>>`.
            // `never` keeps the union whole where no collection names its value type, and judges nothing there.
            let value_type = TUnion::from_vec(
                collections
                    .clone()
                    .filter_map(|array| match (is_map_literal, array) {
                        (true, TArray::Keyed(TKeyedArray { parameters: Some((_, value)), .. })) => Some(&value.types),
                        (false, TArray::List(list)) => Some(&list.element_type.types),
                        (false, TArray::Set(element)) => Some(&element.types),
                        _ => None,
                    })
                    .flat_map(|types| types.iter().cloned())
                    .chain([TAtomic::Never])
                    .collect(),
            );
            for element in &literal.elements {
                let element_value = match element {
                    ArrayElement::KeyValue(element) => element.value,
                    ArrayElement::Value(element) => element.value,
                    _ => continue,
                };
                refused |= check_sharp_literal_kind(context, block_context, artifacts, element_value, &value_type);
            }
            continue;
        }

        // No collection takes the literal, so the first is of a kind it cannot reach, and names the literal to write.
        let message = match (is_map_literal, collection, literal.elements.is_empty()) {
            (false, TArray::Keyed(_), true) => "`[]` is an empty List. An empty Map is written `[:]`.",
            (false, TArray::Keyed(_), false) => "A Map literal is written `[key: value]`.",
            (true, TArray::List(_), true) => "`[:]` is an empty Map. An empty List is written `[]`.",
            (true, TArray::List(_), false) => "A List literal is written `[a, b]`.",
            (true, TArray::Set(_), true) => "`[:]` is an empty Map. An empty Set is written `[]`.",
            (true, TArray::Set(_), false) => "A Set literal is written `[a, b]`.",
            _ => continue,
        };

        let declared_str = display_type(context, declared);
        context.collector.report_with_code(
            IssueCode::InvalidArrayElement,
            Issue::error(message).with_annotation(
                Annotation::primary(literal.span()).with_message(format!("Declared `{declared_str}`.")),
            ),
        );
        refused = true;
    }

    refused
}

/// Refuses `value`, a `List` that is no literal, where `declared` takes only a `Map`, spec section 12, unless plain PHP
/// gives it.
fn report_list_in_map_place<'arena, A>(
    context: &mut Context<'_, 'arena, A>,
    block_context: Option<&BlockContext<'_>>,
    artifacts: &AnalysisArtifacts,
    value: &Expression<'arena>,
    declared: &TUnion,
) where
    A: Arena,
{
    let declared_type =
        block_context.and_then(|block_context| get_declared_type(context, block_context, artifacts, value));
    let is_list = |atomic: &TAtomic| matches!(atomic, TAtomic::Array(TArray::List(list)) if list.tuple_names.is_none());
    let is_list_value =
        declared_type.as_ref().or_else(|| artifacts.get_expression_type(value)).is_some_and(|value_type| {
            value_type.types.iter().any(is_list)
                && value_type.types.iter().all(|atomic| is_list(atomic) || atomic.is_null())
        });
    if !is_list_value || is_plain_php_value(context, artifacts, value) {
        return;
    }

    let declared_str = display_type(context, declared);
    context.collector.report_with_code(
        IssueCode::InvalidArrayElement,
        Issue::error("A `List` is not a `Map`: its keys are only its positions.")
            .with_annotation(Annotation::primary(value.span()).with_message(format!("Declared `{declared_str}`.")))
            .with_help("Declare a `List`, or build the `Map` from it, as in `items.associateBy(item => item.id)`."),
    );
}

/// Whether plain PHP gives `value`: the result of a call of a plain PHP function or method, or a read of a property a
/// plain PHP class declares. The prelude's stubs of the engine's `Sharp` classes are PHP#'s own.
pub(crate) fn is_plain_php_value<A>(
    context: &Context<'_, '_, A>,
    artifacts: &AnalysisArtifacts,
    value: &Expression<'_>,
) -> bool
where
    A: Arena,
{
    let is_plain_class = |class: &[u8]| {
        !class.get(..6).is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"sharp\\"))
            && context.codebase.get_class_like(word(class)).is_some_and(|class| !class.flags.is_sharp())
    };

    match value.unparenthesized() {
        Expression::Call(_) => artifacts.get_callees(value).any(|target| match target {
            CallTarget::FunctionLike { callee, .. } => {
                let owner = match callee {
                    FunctionLikeIdentifier::Method(class, _) => class.as_bytes(),
                    _ => b"",
                };
                context.codebase.get_function_like(callee).is_some_and(|function| !function.flags.is_sharp())
                    && !owner.get(..6).is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"sharp\\"))
            }
            _ => false,
        }),
        Expression::Access(Access::Property(PropertyAccess {
            object,
            property: ClassLikeMemberSelector::Identifier(name),
            ..
        })) => {
            let property = [b"$", name.value].concat();
            artifacts.get_expression_type(object).is_some_and(|object_type| {
                object_type.types.iter().any(|atomic| match atomic {
                    TAtomic::Object(TObject::Named(object)) => context
                        .codebase
                        .get_declaring_property_class(object.name, word(&property))
                        .is_some_and(|declaring| is_plain_class(declaring.as_bytes())),
                    _ => false,
                })
            })
        }
        _ => false,
    }
}

/// Whether a PHP# literal is a `Map` literal, `[:]` or `[key: value]`, rather than a `List` literal, `[]` or `[a, b]`.
/// `None` for a literal of spreads alone, which names no collection of its own.
fn is_map_literal(literal: &Array<'_>) -> Option<bool> {
    match literal.elements.iter().find(|element| element.is_key_value() || element.is_value()) {
        Some(element) => Some(element.is_key_value()),
        None if literal.elements.is_empty() => Some(literal.colon.is_some()),
        None => None,
    }
}

fn handle_variadic_array_element<'arena, A>(
    context: &mut Context<'_, 'arena, A>,
    array_creation_info: &mut ArrayCreationInfo,
    variadic_array_element: &VariadicArrayElement<'arena>,
    variadic_array_element_type: &TUnion,
) where
    A: Arena,
{
    let mut all_non_empty = true;

    for atomic_type in variadic_array_element_type.types.as_ref() {
        let (key_type, value_type) = match atomic_type {
            TAtomic::Array(array_type) => match array_type {
                TArray::Keyed(keyed_data) => {
                    // A PHP# `Map` spread keeps every key, as `array_replace` does, where PHP renumbers int keys.
                    let keeps_keys = context.dialect.is_sharp();
                    if keeps_keys {
                        array_creation_info.is_list = false;
                    }

                    if let Some(known_items) = &keyed_data.known_items {
                        for (key, (possibly_undefined, value_type)) in known_items {
                            if *possibly_undefined {
                                continue;
                            }

                            let new_offset_key = match key {
                                ArrayKey::Integer(key) if keeps_keys => {
                                    array_creation_info
                                        .item_key_atomic_types
                                        .push(TAtomic::Scalar(TScalar::literal_int(*key)));

                                    ArrayKey::Integer(*key)
                                }
                                ArrayKey::Integer(_) => {
                                    if array_creation_info.int_offset == i64::MAX {
                                        report_unpacked_int_key_overflow(context, variadic_array_element.span());

                                        continue;
                                    }

                                    array_creation_info.int_offset += 1;
                                    array_creation_info
                                        .item_key_atomic_types
                                        .push(TAtomic::Scalar(TScalar::literal_int(array_creation_info.int_offset)));

                                    ArrayKey::Integer(array_creation_info.int_offset)
                                }
                                ArrayKey::String(string_key) => {
                                    array_creation_info.is_list = false;
                                    array_creation_info
                                        .item_key_atomic_types
                                        .push(TAtomic::Scalar(TScalar::String(TString::known_literal(*string_key))));
                                    ArrayKey::String(*string_key)
                                }
                                ArrayKey::ClassLikeConstant { .. } => {
                                    // unresolved class-like constant key; treat as opaque
                                    array_creation_info.is_list = false;
                                    *key
                                }
                            };

                            array_creation_info.property_types.insert(new_offset_key, (false, value_type.clone()));

                            if new_offset_key.is_integer() {
                                array_creation_info.array_keys.insert(new_offset_key);
                            } else {
                                array_creation_info.known_non_list = true;
                            }
                        }
                    }

                    // Update non-empty status
                    if !keyed_data.non_empty {
                        all_non_empty = false;
                    }

                    match keyed_data.get_generic_parameters() {
                        Some(parameters) => {
                            // Keys beyond the known ones make the literal a `Map` of the spread's key type.
                            if keeps_keys {
                                array_creation_info.can_create_objectlike = false;
                            }

                            (Some(Cow::Borrowed(parameters.0)), Cow::Borrowed(parameters.1))
                        }
                        None => {
                            continue;
                        }
                    }
                }
                TArray::List(list_data) => {
                    let has_unknown_count = list_data.known_count.is_none();
                    if has_unknown_count {
                        array_creation_info.known_int_offset = false;
                    }

                    if let Some(known_elements) = &list_data.known_elements {
                        for (possibly_undefined, value_type) in known_elements.values() {
                            if *possibly_undefined {
                                continue;
                            }

                            if array_creation_info.int_offset == i64::MAX {
                                report_unpacked_int_key_overflow(context, variadic_array_element.span());

                                continue;
                            }

                            array_creation_info.int_offset += 1;
                            let new_key = ArrayKey::Integer(array_creation_info.int_offset);
                            array_creation_info.array_keys.insert(new_key);
                            array_creation_info
                                .item_key_atomic_types
                                .push(TAtomic::Scalar(TScalar::literal_int(array_creation_info.int_offset)));
                            array_creation_info.property_types.insert(new_key, (false, value_type.clone()));
                        }
                    }

                    if !list_data.non_empty {
                        all_non_empty = false;
                    }

                    (None, Cow::Borrowed(list_data.get_element_type()))
                }
                // A PHP# `Set` spread is refused above. Plain PHP spreads the `Sharp\Set` object, which gives its
                // elements at positions 0 to n, as a list does.
                TArray::Set(element_type) => {
                    all_non_empty = false;

                    (None, Cow::Borrowed(element_type.as_ref()))
                }
            },
            atomic => {
                all_non_empty = false;

                let Some((iterable_key, iterable_value)) = get_iterable_parameters(atomic, context.codebase) else {
                    array_creation_info.can_create_objectlike = false;
                    array_creation_info.item_key_atomic_types.push(TAtomic::Scalar(TScalar::ArrayKey));
                    array_creation_info.item_value_atomic_types.push(TAtomic::Mixed(TMixed::new()));

                    let atomic_str = display_atomic(context, atomic);

                    context.collector.report_with_code(
                        IssueCode::InvalidArrayElement,
                        Issue::error(format!("Cannot use spread operator on non-iterable type `{atomic_str}`."))
                        .with_annotation(
                            Annotation::primary(variadic_array_element.span())
                                .with_message("Spread operator requires an iterable type.")
                        )
                        .with_note(
                            "The spread operator (`...`) can only be used with arrays or objects implementing the `Traversable` interface."
                        )
                        .with_help("Consider using an array or a traversable object."),
                    );

                    continue;
                };

                if iterable_value.is_never() {
                    continue;
                }

                array_creation_info.can_create_objectlike = false;

                (Some(Cow::Owned(iterable_key)), Cow::Owned(iterable_value))
            }
        };

        if value_type.is_never() {
            continue;
        }

        if let Some(key_type) = key_type {
            if key_type.is_never() {
                continue;
            }

            // A PHP# `Map` spread may hold any key a `Map` literal takes. Plain PHP spreads into a PHP array, whose
            // keys are `int` or `string` alone.
            let is_sharp = context.dialect.is_sharp();
            let stored_key_type = if is_sharp {
                get_backing_key_type(&key_type, context.codebase, context.dialect)
            } else {
                Cow::Borrowed(&*key_type)
            };
            let is_contained = |container: &TUnion| {
                union_comparator::is_contained_by(
                    context.codebase,
                    &stored_key_type,
                    container,
                    false,
                    false,
                    false,
                    &mut ComparisonResult::for_dialect(context.dialect),
                )
            };
            let is_string_key = is_contained(&get_string());

            let is_array_key_key = if is_sharp {
                is_key_type(&key_type, context.codebase)
            } else {
                is_string_key || is_contained(&get_arraykey())
            };

            if is_string_key {
                array_creation_info.is_list = false;
            }

            if !is_array_key_key {
                let key_type_str = display_type(context, &key_type);

                context.collector.report_with_code(
                    IssueCode::InvalidArrayElementKey,
                    Issue::error(format!("Cannot use spread operator on an iterable with key type `{key_type_str}`."))
                    .with_annotation(
                        Annotation::primary(variadic_array_element.span())
                            .with_message("Spread operator requires an iterable type with array-key keys.")
                    )
                    .with_note(
                        "The spread operator (`...`) can only be used with arrays or objects implementing the `Traversable` interface that have keys of type `array-key`."
                    )
                    .with_help("Consider using an array or a traversable object with appropriate key types.")
                );

                continue;
            }

            for (k, v) in &mut array_creation_info.property_types {
                let prop_key = k.to_union();

                if is_contained_by(
                    context.codebase,
                    &prop_key,
                    &key_type,
                    false,
                    false,
                    false,
                    &mut ComparisonResult::for_dialect(context.dialect),
                ) {
                    let new_prop_val =
                        combine_union_types(&v.1, &value_type, context.codebase, context.settings.combiner_options());

                    *v = (v.0, new_prop_val);
                }
            }

            array_creation_info.item_key_atomic_types.extend(key_type.into_owned().types.into_owned());
        }

        array_creation_info.item_value_atomic_types.extend(value_type.into_owned().types.into_owned());
    }

    if all_non_empty {
        array_creation_info.can_be_empty = false;
    }
}

#[cfg(test)]
mod tests {
    use indoc::indoc;

    use crate::code::IssueCode;
    use crate::test_analysis;

    test_analysis! {
        name = array_literal_empty_square_brackets,
        code = indoc! {"
            <?php

            /**
             * @param array{} $_arr
             */
            function expect_empty_array(array $_arr): void {}

            $empty1 = [];
            expect_empty_array($empty1);
        "}
    }

    test_analysis! {
        name = array_literal_empty_array_keyword,
        code = indoc! {"
            <?php

            /**
             * @param array{} $_arr
             */
            function expect_empty_array(array $_arr): void {}

            $empty2 = array();
            expect_empty_array($empty2);
        "}
    }

    test_analysis! {
        name = array_literal_literal_int_keys_as_list,
        code = indoc! {r#"
            <?php

            /**
             * @param list<string> $_arr
             */
            function expect_list_of_strings(array $_arr): void {}

            $arr = [0 => "a", 1 => "b"];
            expect_list_of_strings($arr);
        "#}
    }

    test_analysis! {
        name = array_literal_literal_string_keys,
        code = indoc! {r#"
            <?php

            /**
             * @param array{"name": string, "age": int} $_arr
             */
            function expect_shape_person(array $_arr): void {}

            $arr = ["name" => "Alice", "age" => 30];
            expect_shape_person($arr);
        "#}
    }

    test_analysis! {
        name = array_literal_numeric_string_keys_coerced_to_int,
        code = indoc! {r#"
            <?php

            /**
             * @param array{123: bool, 456: bool} $_arr
             */
            function expect_numeric_string_keys(array $_arr): void {}

            $arr = ["123" => true, "456" => false];
            expect_numeric_string_keys($arr);
        "#}
    }

    test_analysis! {
        name = array_literal_null_key_becomes_empty_string,
        code = indoc! {r#"
            <?php

            /**
             * @param array{"": string} $_arr
             */
            function expect_null_key(array $_arr): void {}

            $arr = [null => "val_for_empty_string_key"];
            expect_null_key($arr);
        "#}
    }

    test_analysis! {
        name = array_literal_bool_keys_coerced_to_int,
        code = indoc! {r#"
            <?php

            /**
             * @param array{1: string, 0: string} $_arr
             */
            function expect_bool_keys(array $_arr): void {}

            $arr = [true => "is_true", false => "is_false"];
            expect_bool_keys($arr);
        "#}
    }

    test_analysis! {
        name = array_literal_float_keys_truncated_to_int,
        code = indoc! {r#"
            <?php

            /**
             * @param array{1: string, 2: string} $_arr
             */
            function expect_float_keys(array $_arr): void {}

            $arr = [1.7 => "val1", 2.0 => "val2"];
            expect_float_keys($arr);
        "#}
    }

    test_analysis! {
        name = array_literal_duplicate_literal_int_key,
        code = indoc! {"
            <?php

            /**
             * @param array{0: string} $_arr
             */
            function expect_single_string_at_0(array $_arr):void{}

            $arr = [0 => 'a', 0 => 'b'];
            expect_single_string_at_0($arr);
        "},
        issues = [IssueCode::DuplicateArrayKey]
    }

    test_analysis! {
        name = array_literal_duplicate_literal_string_key,
        code = indoc! {r#"
            <?php

            /**
             * @param array{"k":string} $_arr
             */
            function expect_k_string(array $_arr):void{}

            $arr = ["k" => 'a', "k" => 'b'];
            expect_k_string($arr);
        "#},
        issues = [IssueCode::DuplicateArrayKey]
    }

    test_analysis! {
        name = array_literal_duplicate_coerced_key,
        code = indoc! {"
            <?php

            /**
             * @param array{1:string} $_arr
             */
            function expect_one_string(array $_arr):void{}

            $arr = [true => 'a', 1 => 'b']; // true becomes 1, so '1' is duplicated
            expect_one_string($arr);
        "},
        issues = [IssueCode::DuplicateArrayKey]
    }

    test_analysis! {
        name = array_literal_simple_list_implicit_keys,
        code = indoc! {r#"
            <?php

            /** @param list<string> $_arr */
            function expect_list_of_strings(array $_arr): void {}

            $arr = ["alpha", "beta", "gamma"];
            expect_list_of_strings($arr);
        "#}
    }

    test_analysis! {
        name = array_literal_mixed_implicit_explicit_keys_maintaining_list,
        code = indoc! {r#"
            <?php

            /** @param array{0: string, 1: string, 2: string} $_arr */
            function expect_shape_list_three_strings(array $_arr): void {}

            $arr = [0 => "a", "b", 2 => "c"];
            expect_shape_list_three_strings($arr);
        "#}
    }

    test_analysis! {
        name = array_literal_mixed_implicit_explicit_keys_breaking_list,
        code = indoc! {r#"
            <?php

            /** @param array<array-key, string> $_arr */
            function expect_general_keyed_array_string_values(array $_arr): void {}

            $arr = ["key" => "a", "b"]; // "b" gets key 0, becomes keyed array
            expect_general_keyed_array_string_values($arr);
        "#}
    }

    test_analysis! {
        name = array_literal_spread_empty_array,
        code = indoc! {"
            <?php

            /** @param array{} $_arr */
            function expect_empty_array(array $_arr): void {}

            $empty = [];
            $arr = [...$empty];
            expect_empty_array($arr);
        "}
    }

    test_analysis! {
        name = array_literal_spread_list,
        code = indoc! {r#"
            <?php

            /** @param list<string> $_arr */
            function expect_list_of_strings(array $_arr): void {}

            $list = ["a", "b"];
            $arr = [...$list, "c"];
            expect_list_of_strings($arr); // Expects list<string>
        "#}
    }

    test_analysis! {
        name = array_literal_spread_keyed_array,
        code = indoc! {r#"
            <?php

            /** @param array<string, int> $_arr */
            function expect_map_string_to_int(array $_arr): void {}

            $keyed = ["x" => 1, "y" => 2];
            $arr = [...$keyed, "z" => 3];
            expect_map_string_to_int($arr);
        "#}
    }

    test_analysis! {
        name = array_literal_spread_non_iterable_int,
        code = indoc! {"
            <?php

            /** @param array<array-key, mixed> $_arr */
            function expect_empty_array(array $_arr): void {}

            $num = 123;
            $arr = [...$num];
            expect_empty_array($arr);
        "},
        issues = [IssueCode::InvalidArrayElement]
    }

    test_analysis! {
        name = array_literal_missing_element,
        code = indoc! {"
            <?php

            $arr = [1, , 3];
        "},
        issues = [IssueCode::InvalidArrayElement]
    }

    test_analysis! {
        name = array_literal_results_in_list_type,
        code = indoc! {"
            <?php

            /** @param list<int> $_arr */
            function expect_list_of_ints(array $_arr): void {}

            $arr = [10, 20, 30];
            expect_list_of_ints($arr);
        "}
    }

    test_analysis! {
        name = array_literal_results_in_keyed_array_string_keys,
        code = indoc! {r#"
            <?php

            /** @param array<string, bool> $_arr */
            function expect_map_string_to_bool(array $_arr): void {}

            $arr = ["a" => true, "b" => false];
            expect_map_string_to_bool($arr);
        "#}
    }

    test_analysis! {
        name = array_literal_results_in_keyed_array_mixed_keys,
        code = indoc! {r#"
            <?php

            /** @param array<array-key, int> $_arr */
            function expect_general_keyed_array_int_values(array $_arr): void {}

            $arr = [0 => 1, "a" => 2, 1 => 3];
            expect_general_keyed_array_int_values($arr);
        "#}
    }

    test_analysis! {
        name = list_array_comparison,
        code = indoc! {"
            <?php

            /**
             * @return array{0: 1, 1: 2, 2: 3, 3: 4, 4: 5}
             */
            function x(): array {
               exit();
            }

            /**
             * @param list<1|2|3|4|5> $_list
             */
            function take_list_ints(array $_list): void {
            }

            $list = x();
            take_list_ints($list);
        "}
    }

    test_analysis! {
        name = create_array_using_generic_key,
        code = indoc! {"
            <?php

            /**
             * @template K of array-key
             * @template V
             *
             * @param K $k
             * @param V $v
             *
             * @return non-empty-array<K, V>
             */
            function create_array($k, $v): array {
                return [$k => $v];
            }
        "},
    }
}
