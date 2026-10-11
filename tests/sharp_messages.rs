//! Reads the tests of `.sharp` messages and checks the two promises they make: a `.sharp` message speaks PHP#, so
//! it names nothing only PHP has, and a test of one asserts the `.php` twin as well, so a `.sharp` rule leaves PHP's
//! messages alone.
//!
//! A test's expected strings are what it asserts, so the checks read them from the test source rather than running
//! the analyzer. A string's dialect is that of the first file path in the innermost part of its statement that names
//! one, which is the file under test, since every helper takes the analyzed file before the others.

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::LazyLock;

use proc_macro2::Delimiter;
use proc_macro2::Spacing;
use proc_macro2::TokenStream;
use proc_macro2::TokenTree;
use regex::Regex;
use syn::Lit;

/// The test files that assert `.sharp` messages.
const MESSAGE_TESTS: [(&str, &str); 2] = [
    ("crates/analyzer/tests/sharp.rs", include_str!("../crates/analyzer/tests/sharp.rs")),
    ("crates/semantics/tests/sharp.rs", include_str!("../crates/semantics/tests/sharp.rs")),
];

/// Tests of `.sharp` messages about code PHP cannot write, so they have no `.php` twin, under the reason PHP cannot.
const PHP_CANNOT_EXPRESS: &[(&str, &[&str])] = &[
    (
        "PHP has no effects, `extern` declarations or laws, and no getter it holds pure.",
        &[
            "a_call_of_a_law_is_a_non_existent_method_that_names_the_law",
            "a_bare_member_in_a_law_is_written_through_the_class_name",
            "a_function_type_uses_only_effects",
            "a_getter_applying_an_operator_whose_body_has_an_effect_is_refused",
            "a_getter_calling_a_class_its_file_does_not_import_names_the_import_the_extern_needs",
            "a_getter_calling_a_class_of_its_own_namespace_names_no_import",
            "a_getter_calling_a_class_whose_short_name_its_file_imports_names_an_aliased_import",
            "a_getter_calling_a_function_with_no_extern_is_refused_and_names_the_missing_declaration",
            "a_getter_calling_a_method_a_magic_method_serves_has_the_effects_of_the_magic_method",
            "a_getter_calling_a_method_of_a_class_imported_under_another_name_names_the_class_by_that_name",
            "a_getter_calling_a_method_of_a_class_that_shares_its_short_name_names_the_class_in_full",
            "a_getter_calling_a_plain_php_property_holding_a_function_has_an_unknown_effect",
            "a_getter_calling_a_plain_php_property_of_a_class_that_shares_its_short_name_names_the_class_in_full",
            "a_getter_calling_a_virtual_method_whose_override_has_an_effect_is_refused",
            "a_getter_reaching_a_call_in_another_file_names_the_class_as_the_getter_file_does",
            "a_getter_reaching_a_method_of_a_php_sharp_class_that_shares_its_short_name_names_the_class_in_full",
            "a_getter_reaching_a_method_that_calls_an_extern_with_an_effect_in_another_file_is_refused",
            "a_getter_reading_the_environment_is_refused_with_the_effect_environment",
            "a_getter_whose_list_map_lambda_calls_an_undeclared_function_is_refused",
            "a_getter_writing_a_field_of_this_is_refused",
            "a_law_and_a_method_named_alike_are_a_duplicate_member",
            "a_law_calling_a_plain_php_property_holding_a_closure_has_an_unknown_effect",
            "a_law_calling_plain_php_with_an_effect_is_refused_on_the_call",
            "a_law_calling_plain_php_with_no_extern_names_the_missing_declaration",
            "a_law_is_a_member_of_a_class_or_an_enum_and_of_no_interface",
            "a_law_parameter_with_a_default_or_a_spread_is_refused",
            "a_law_parameter_without_a_type_is_refused_as_any_php_sharp_parameter_is",
            "a_law_reaching_a_method_with_an_effect_in_another_file_is_refused",
            "a_law_whose_body_is_not_a_bool_is_an_invalid_return_statement_of_the_law",
            "a_recursive_pair_of_methods_solves_without_looping",
            "a_second_extern_of_a_target_in_another_file_is_refused_on_the_second",
            "an_aliased_import_capitalizes_each_part_of_a_lowercase_namespace",
            "an_aliased_import_whose_alias_the_file_binds_takes_the_first_free_number",
            "an_effect_name_an_import_shadows_is_refused_with_the_imported_class",
            "an_extern_method_in_a_project_file_is_an_error_in_any_namespace",
            "an_extern_method_is_public_static_in_a_static_class_with_no_body_in_any_namespace",
            "an_extern_method_with_a_body_has_its_body_checked",
            "an_extern_of_a_php_sharp_class_is_refused",
            "an_extern_uses_only_effects",
            "an_extern_written_with_a_renamed_import_declares_the_same_target",
            "extern_is_a_declaration_of_the_file_and_of_no_method_body",
            "extern_on_a_field_is_not_supported_yet",
        ],
    ),
    (
        "PHP has one `array` type, so it cannot write a rule of `List`, `Map` or `Set`.",
        &[
            "a_bare_map_read_names_the_forms_that_handle_a_missing_key",
            "a_collection_method_message_names_the_sharp_type",
            "a_compound_assignment_or_increment_on_an_index_names_the_form_that_compiles",
            "a_function_value_keeps_its_parameter_collections_through_a_copy_a_template_and_a_list",
            "a_list_literal_where_a_map_is_declared_is_an_error",
            "a_literal_of_the_other_collection_passed_to_a_function_local_is_an_error",
            "a_literal_of_the_other_collection_passed_to_a_function_parameter_is_an_error",
            "a_literal_of_the_other_collection_passed_to_a_function_property_is_an_error",
            "a_literal_of_the_other_collection_passed_to_a_lambda_is_an_error",
            "a_literal_of_the_other_collection_passed_to_a_method_value_is_an_error",
            "a_loop_over_a_map_whose_keys_mix_a_backed_enum_with_other_types_is_an_error",
            "a_map_key_is_refused_once",
            "a_map_key_that_is_not_int_string_or_a_named_type_is_an_error",
            "a_map_key_type_without_a_backing_value_is_an_error",
            "a_map_literal_added_to_a_list_of_lists_is_an_error",
            "a_map_literal_set_into_a_list_of_lists_is_an_error",
            "a_map_literal_where_a_list_is_declared_is_an_error",
            "a_set_of_an_element_other_than_int_string_or_a_named_type_is_not_supported_yet",
            "a_type_argument_takes_a_question_mark_in_a_field",
            "a_type_with_type_arguments_other_than_list_map_or_set_is_not_supported_yet",
            "a_union_of_two_collections_or_two_function_types_is_a_type_written_twice",
            "a_value_that_may_be_a_function_value_or_a_php_closure_of_one_signature_keeps_its_parameter_collections",
            "an_empty_list_literal_where_a_map_is_declared_is_an_error",
            "an_empty_literal_needs_a_type",
            "an_empty_map_literal_where_a_list_is_declared_is_an_error",
            "an_interface_method_takes_and_returns_list_and_map_types",
            "equality_of_a_collection_is_not_supported_yet",
            "list_wrap_of_a_list_is_refused_with_the_is_form_that_decides_it",
            "list_wrap_of_a_map_literal_is_refused_once",
            "list_wrap_of_a_map_or_a_list_of_lists_is_refused",
            "list_wrap_of_a_value_of_any_type_is_refused",
            "sorted_by_on_a_map_a_set_and_a_chain_refuses_a_key_with_no_order",
        ],
    ),
    (
        "PHP declares a variable by assigning it, for its whole function, with no type and no `const`.",
        &[
            "a_catch_variable_is_out_of_scope_after_its_catch_block",
            "a_const_loop_variable_cannot_be_assigned_and_is_out_of_scope_after_its_loop",
            "a_for_counter_is_out_of_scope_after_its_loop_and_const_when_declared_so",
            "a_local_declared_in_a_loop_body_is_out_of_scope_after_it",
            "a_local_declared_in_an_if_branch_is_out_of_scope_after_it",
            "a_null_start_needs_a_type",
            "a_typed_for_counter_takes_the_types_of_the_slice_but_not_void",
            "a_typed_local_takes_the_types_of_the_slice_but_not_void",
            "a_typed_loop_variable_takes_the_types_of_the_slice_but_not_void",
            "a_value_that_is_not_the_written_type_of_a_local_is_reported",
            "incrementing_or_decrementing_a_const_local_is_an_error",
            "null_coalescing_assignment_to_a_const_local_is_an_error",
            "reassigning_a_const_local_is_an_error",
            "reassigning_a_const_local_spelled_in_capitals_is_an_error",
            "redeclaring_a_name_an_enclosing_block_declares_is_an_error",
            "using_a_local_after_its_block_closes_is_an_error",
            "writing_a_const_local_after_its_block_closes_reports_only_the_scope_error",
        ],
    ),
    (
        "PHP has no `is` patterns, `when` guards or `as` conversions.",
        &[
            "a_match_whose_arms_name_class_values_leaves_its_default_to_the_analyzer",
            "a_nullable_type_pattern_names_the_null_check_to_write",
            "a_nullable_type_pattern_or_as_to_a_nullable_type_is_an_error",
            "a_pattern_the_parser_refuses_reports_only_its_parse_error",
            "a_pattern_variable_named_this_is_an_error",
            "a_pattern_variable_under_or_or_not_is_an_error_but_under_the_not_that_starts_is",
            "a_pattern_variable_used_where_its_test_does_not_hold_names_its_test",
            "a_when_condition_that_is_not_bool_is_an_invalid_operand_named_when",
            "not_beside_or_in_a_pattern_needs_parentheses_and_not_beside_and_does_not",
        ],
    ),
    (
        "PHP has no operator overloading.",
        &[
            "a_subclass_cannot_declare_an_operator_its_parent_declares",
            "an_interface_or_an_enum_declares_no_operator",
            "an_operator_a_class_cannot_declare_is_an_error_that_names_the_operators_it_derives_from",
            "an_operator_body_is_a_method_body",
            "an_operator_declared_twice_in_a_class_is_an_error",
            "an_operator_on_an_instance_whose_class_declares_none_is_refused_by_name",
            "an_operator_that_is_not_public_static_is_an_error",
            "an_operator_with_the_wrong_number_of_parameters_is_an_error",
            "an_operator_without_its_class_as_a_parameter_is_an_error",
            "an_undeclared_operator_and_a_wrong_operand_are_refused",
            "equality_needs_a_public_int_hash_in_the_class_or_a_parent",
            "equality_returns_bool_and_comparison_returns_int",
            "ordering_a_nullable_instance_is_refused_until_it_is_tested_for_null",
        ],
    ),
    (
        "PHP has no static class: it refuses the `static` modifier on a class.",
        &[
            "a_class_that_extends_a_static_class_is_an_error",
            "a_member_of_a_static_class_that_is_not_static_is_an_error",
            "a_static_class_cannot_extend_a_class_or_implement_an_interface",
            "a_static_class_has_no_constructor",
            "new_on_a_static_class_is_an_error",
        ],
    ),
    (
        "PHP has no `virtual`, `override`, `required` or `super`, and no `Self` type.",
        &[
            "a_member_of_self_read_or_written_is_not_supported_yet",
            "a_method_opens_with_virtual_and_replaces_with_override",
            "a_property_with_accessor_bodies_that_overrides_a_parent_property_is_not_supported_yet",
            "an_enum_returns_self_holds_union_constants_and_refuses_new_self",
            "an_override_whose_initial_value_is_not_constant_is_an_error",
            "an_override_without_an_initial_value_is_an_error",
            "an_overriding_property_is_not_supported_yet",
            "new_self_needs_a_required_constructor",
            "required_is_a_modifier_of_the_constructor_only",
            "self_in_a_function_type_is_an_error_and_a_list_of_self_is_a_return_type",
            "self_is_only_a_return_type",
            "super_calls_the_parent_method",
        ],
    ),
    (
        "PHP writes property hooks, not accessors with `field`, a `get;` or a computed `=>` property.",
        &[
            "a_get_only_nullable_property_without_an_initial_value_is_not_supported_yet",
            "a_get_only_nullable_property_without_an_initial_value_names_the_initial_value_or_set_to_write",
            "a_promoted_member_follows_the_rules_of_the_same_declaration_in_the_class_body",
            "a_static_computed_property_or_one_without_an_access_modifier_is_not_supported_yet",
            "an_auto_property_breaking_the_accessor_rules_is_an_error",
            "an_initial_value_that_is_not_constant_on_a_property_whose_set_has_a_body_is_not_supported_yet",
            "field_in_a_nested_block_gives_storage_and_field_only_in_a_lambda_does_not",
            "field_inside_a_lambda_in_an_accessor_is_an_error",
            "properties_outside_the_slice_are_not_supported_yet",
        ],
    ),
    (
        "PHP has no `Function<…>` type: a closure's type is `\\Closure`, whatever it takes.",
        &[
            "a_sharp_message_names_a_function_type_with_its_effects_inside_its_brackets",
            "a_void_parameter_of_a_function_type_is_not_supported_yet",
        ],
    ),
    (
        "PHP chooses each closure's capture, by value in `fn` and through `use`, so no capture is in doubt.",
        &[
            "a_lambda_capturing_a_loop_variable_that_changes_is_not_supported_yet",
            "an_enum_method_takes_collection_and_function_types_and_holds_lambdas_as_a_class_method_does",
        ],
    ),
    (
        "PHP writes a property read, `->total`, apart from a call, `->total()`, so a member's kind is never in doubt.",
        &["a_member_whose_kind_differs_across_the_receivers_classes_is_an_error"],
    ),
    (
        "PHP reads a class's member with `::`, which has no null-safe form.",
        &["null_safe_access_on_a_class_is_an_error_that_names_the_dot"],
    ),
    (
        "PHP cannot parse a type in parentheses with `?` after it.",
        &["a_single_type_in_parentheses_with_a_question_mark_after_it_reports_only_the_php_errors"],
    ),
];

/// What a `.sharp` message must not name, each with the pattern that finds it.
static BANNED: LazyLock<Vec<(&str, Regex)>> = LazyLock::new(|| {
    [
        ("`mixed`", r"\bmixed\b"),
        ("the word `unit`", r"(?i)\bunit\b"),
        ("`isset`", r"\bisset\b"),
        ("a collection stub class", r"\b(List|Map|Set)Methods\b"),
        ("a PHP class name with a backslash", r"\w\\\w"),
        ("PHP member access", r"(->|::)\$?[A-Za-z_]"),
        ("a spec section", r"(?i)\bsections?\s+\d|§"),
    ]
    .into_iter()
    .map(|(name, pattern)| (name, Regex::new(pattern).expect("a valid pattern")))
    .collect()
});

/// The PHP form a refusal quotes because the developer wrote it, as in "PHP# has no `mixed`".
static REFUSED_FORM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"PHP# has no `[^`]*`").expect("a valid pattern"));

/// A file path a test names: the analyzed file or one beside it.
static PATH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[\w./-]+\.(sharp|php)$").expect("a valid pattern"));

/// What leads a rendered issue before its message: the file of an effect issue, its position and its code.
static LEAD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:[\w./-]+\.(sharp|php):)?(?:\d+:\d+ ?)?(?:[a-z0-9]+(?:-[a-z0-9]+)+:?(?: |$))?")
        .expect("a valid pattern")
});

/// An issue code, alone.
static CODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9]+(-[a-z0-9]+)+$").expect("a valid pattern"));

/// A placeholder of `format!`, named or not.
static PLACEHOLDER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{([A-Za-z_][A-Za-z0-9_]*)?(:[^{}]*)?\}").expect("a valid pattern"));

/// The issue fields that hold its words.
const WORDED_FIELDS: [&str; 4] = ["message", "help", "notes", "annotations"];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dialect {
    Sharp,
    Php,
}

impl Dialect {
    fn of_extension(extension: &str) -> Self {
        if extension == "sharp" { Self::Sharp } else { Self::Php }
    }
}

/// A string a test asserts, with the dialect of the file it was rendered from.
struct Expected {
    text: String,
    dialect: Option<Dialect>,
}

/// One test of a message test file.
struct Test {
    name: String,
    /// The strings the test asserts.
    expected: Vec<Expected>,
    /// Whether one of its assertions pins what a `.php` file reports: its words, or no issue at all.
    asserts_php: bool,
}

/// One message test file: its helpers and constants by name, and its tests.
struct TestFile {
    items: HashMap<String, Vec<TokenTree>>,
    functions: HashSet<String>,
    renderers: HashSet<String>,
}

type Bindings = HashMap<String, Vec<TokenTree>>;

fn trees(stream: TokenStream) -> Vec<TokenTree> {
    stream.into_iter().collect()
}

fn string(token: &TokenTree) -> Option<String> {
    let TokenTree::Literal(literal) = token else { return None };
    match Lit::new(literal.clone()) {
        Lit::Str(string) => Some(string.value()),
        _ => None,
    }
}

fn is_punct(token: Option<&TokenTree>, character: char) -> bool {
    matches!(token, Some(TokenTree::Punct(punct)) if punct.as_char() == character)
}

fn parenthesized(token: Option<&TokenTree>) -> bool {
    matches!(token, Some(TokenTree::Group(group)) if group.delimiter() == Delimiter::Parenthesis)
}

fn is_block(token: &TokenTree) -> bool {
    matches!(token, TokenTree::Group(group) if group.delimiter() == Delimiter::Brace)
}

/// Splits `tokens` at each `separator` outside a group.
fn split(tokens: &[TokenTree], separator: char) -> Vec<&[TokenTree]> {
    let mut parts = Vec::new();
    let mut start = 0;
    for (index, token) in tokens.iter().enumerate() {
        if is_punct(Some(token), separator) {
            parts.push(&tokens[start..index]);
            start = index + 1;
        }
    }
    parts.push(&tokens[start..]);
    parts
}

/// Splits a block's `tokens` into statements: each ends at a `;`, or at the block of a loop or an `if` with no `else`
/// after it.
fn statements(tokens: &[TokenTree]) -> Vec<&[TokenTree]> {
    let mut statements = Vec::new();
    let mut start = 0;
    for (index, token) in tokens.iter().enumerate() {
        let ends_block = is_block(token)
            && matches!(&tokens[start], TokenTree::Ident(keyword) if ["for", "while", "loop", "if"].contains(&keyword.to_string().as_str()))
            && !matches!(tokens.get(index + 1), Some(TokenTree::Ident(keyword)) if keyword == "else");
        if is_punct(Some(token), ';') || ends_block {
            statements.push(&tokens[start..index + usize::from(ends_block)]);
            start = index + 1;
        }
    }
    statements.push(&tokens[start..]);
    statements
}

/// Every name a `let` binds anywhere in `tokens`, each to the tokens of its value.
fn bindings(tokens: &[TokenTree], into: &mut Bindings) {
    for statement in statements(tokens) {
        if matches!(statement.first(), Some(TokenTree::Ident(keyword)) if keyword == "let") {
            let assign = statement.iter().enumerate().position(|(index, token)| {
                matches!(token, TokenTree::Punct(punct) if punct.as_char() == '=' && punct.spacing() == Spacing::Alone)
                    && !matches!(index.checked_sub(1).map(|before| &statement[before]), Some(TokenTree::Punct(before)) if before.spacing() == Spacing::Joint)
            });
            if let Some(assign) = assign {
                let pattern = split(&statement[1..assign], ':')[0];
                let value = statement[assign + 1..].to_vec();
                for token in pattern {
                    match token {
                        TokenTree::Ident(name) if name != "mut" && name != "ref" => {
                            into.insert(name.to_string(), value.clone());
                        }
                        TokenTree::Group(group) => {
                            for name in trees(group.stream()).iter().filter_map(|token| match token {
                                TokenTree::Ident(name) if name != "mut" && name != "ref" => Some(name.to_string()),
                                _ => None,
                            }) {
                                into.insert(name, value.clone());
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        for token in statement {
            if let TokenTree::Group(group) = token {
                bindings(&trees(group.stream()), into);
            }
        }
    }
}

impl TestFile {
    fn parse(source: &str) -> (Self, Vec<(String, Vec<TokenTree>)>) {
        let top = trees(source.parse::<TokenStream>().expect("a test file is Rust"));
        let mut file = Self { items: HashMap::new(), functions: HashSet::new(), renderers: HashSet::new() };
        let mut tests = Vec::new();
        let mut is_test = false;
        let mut index = 0;
        while index < top.len() {
            match &top[index] {
                TokenTree::Group(attribute) if attribute.delimiter() == Delimiter::Bracket => {
                    is_test |= attribute.stream().to_string() == "test";
                }
                TokenTree::Ident(keyword) if keyword == "fn" => {
                    let name = top[index + 1].to_string();
                    while !is_block(&top[index]) {
                        index += 1;
                    }
                    let TokenTree::Group(body) = &top[index] else { unreachable!() };
                    if is_test {
                        tests.push((name, trees(body.stream())));
                    } else {
                        file.functions.insert(name.clone());
                        file.items.insert(name, trees(body.stream()));
                    }
                    is_test = false;
                }
                TokenTree::Ident(keyword) if keyword == "const" || keyword == "static" => {
                    let name = top[index + 1].to_string();
                    let start =
                        index + top[index..].iter().position(|token| is_punct(Some(token), '=')).expect("a value");
                    let end = start + top[start..].iter().position(|token| is_punct(Some(token), ';')).expect("an end");
                    file.items.insert(name, top[start + 1..end].to_vec());
                    index = end;
                }
                _ => {}
            }
            index += 1;
        }

        loop {
            let found: Vec<String> = file
                .functions
                .iter()
                .filter(|name| !file.renderers.contains(*name) && file.renders(&file.items[*name], &Bindings::new()))
                .cloned()
                .collect();
            if found.is_empty() {
                break;
            }
            file.renderers.extend(found);
        }

        (file, tests)
    }

    /// The value `name` holds where `bindings` are in scope: a `let`, a constant or a static.
    fn binding<'a>(&'a self, name: &str, bindings: &'a Bindings) -> Option<&'a Vec<TokenTree>> {
        bindings.get(name).or_else(|| self.items.get(name).filter(|_| !self.functions.contains(name)))
    }

    /// Whether `tokens` read the words of an issue, themselves or through a helper or a binding.
    fn renders(&self, tokens: &[TokenTree], bindings: &Bindings) -> bool {
        self.renders_seen(tokens, bindings, &mut HashSet::new())
    }

    fn renders_seen(&self, tokens: &[TokenTree], bindings: &Bindings, seen: &mut HashSet<String>) -> bool {
        tokens.iter().enumerate().any(|(index, token)| match token {
            TokenTree::Punct(dot) if dot.as_char() == '.' => {
                matches!(tokens.get(index + 1), Some(TokenTree::Ident(field)) if WORDED_FIELDS.iter().any(|worded| field == worded))
            }
            TokenTree::Ident(name) => {
                let name = name.to_string();
                (self.renderers.contains(&name) && parenthesized(tokens.get(index + 1)))
                    || (seen.insert(name.clone())
                        && bindings.get(&name).is_some_and(|value| self.renders_seen(value, bindings, seen)))
            }
            TokenTree::Group(group) => self.renders_seen(&trees(group.stream()), bindings, seen),
            _ => false,
        })
    }

    /// The dialect of the first file path `tokens` name, themselves or through a helper or a binding.
    fn dialect(&self, tokens: &[TokenTree], bindings: &Bindings) -> Option<Dialect> {
        self.dialect_seen(tokens, bindings, &mut HashSet::new())
    }

    /// A call's arguments come before its callee, whose own body names a file only when it fixes the dialect.
    fn dialect_seen(&self, tokens: &[TokenTree], bindings: &Bindings, seen: &mut HashSet<String>) -> Option<Dialect> {
        let mut index = 0;
        while index < tokens.len() {
            let found = match &tokens[index] {
                token @ TokenTree::Literal(_) => {
                    string(token).and_then(|text| PATH.captures(&text).map(|path| Dialect::of_extension(&path[1])))
                }
                TokenTree::Group(group) => self.dialect_seen(&trees(group.stream()), bindings, seen),
                TokenTree::Ident(name) => {
                    let name = name.to_string();
                    let arguments = match tokens.get(index + 1) {
                        Some(TokenTree::Group(arguments)) if arguments.delimiter() == Delimiter::Parenthesis => {
                            index += 1;
                            self.dialect_seen(&trees(arguments.stream()), bindings, seen)
                        }
                        _ => None,
                    };
                    arguments.or_else(|| {
                        if !seen.insert(name.clone()) {
                            return None;
                        }
                        match bindings.get(&name) {
                            Some(value) => self.dialect_seen(value, bindings, seen),
                            None => {
                                self.items.get(&name).and_then(|item| self.dialect_seen(item, &Bindings::new(), seen))
                            }
                        }
                    })
                }
                TokenTree::Punct(_) => None,
            };
            if found.is_some() {
                return found;
            }
            index += 1;
        }
        None
    }

    /// Every string `tokens` assert, reached through bindings, with the dialect of the file each was rendered from.
    fn expected(
        &self,
        tokens: &[TokenTree],
        inherited: Option<Dialect>,
        bindings: &Bindings,
        seen: &mut HashSet<String>,
        into: &mut Vec<Expected>,
    ) {
        if tokens.first().and_then(string).is_some_and(|text| PATH.is_match(&text)) {
            return;
        }

        let dialect = self.dialect(tokens, bindings).or(inherited);
        let mut index = 0;
        while index < tokens.len() {
            match &tokens[index] {
                token @ TokenTree::Literal(_) => {
                    if let Some(text) = string(token) {
                        for name in PLACEHOLDER.captures_iter(&text).filter_map(|placeholder| placeholder.get(1)) {
                            self.expected_binding(name.as_str(), dialect, bindings, seen, into);
                        }
                        into.push(Expected { text, dialect });
                    }
                }
                TokenTree::Group(group) => self.expected(&trees(group.stream()), dialect, bindings, seen, into),
                TokenTree::Ident(name) => {
                    let name = name.to_string();
                    if name == "for" {
                        // A loop's pattern and the values it walks are inputs: only its body asserts.
                        index += tokens[index..].iter().position(is_block).unwrap_or(0);
                        continue;
                    }
                    let next = tokens.get(index + 1);
                    if is_punct(next, '!') && ["assert", "assert_eq", "assert_ne"].contains(&name.as_str()) {
                        let TokenTree::Group(arguments) = &tokens[index + 2] else { unreachable!() };
                        let arguments = trees(arguments.stream());
                        let dialect = self.dialect(&arguments, bindings).or(dialect);
                        let compared = match name.as_str() {
                            "assert" if !is_punct(arguments.first(), '!') => 1,
                            "assert_eq" => 2,
                            _ => 0,
                        };
                        for argument in split(&arguments, ',').into_iter().take(compared) {
                            self.expected(argument, dialect, bindings, seen, into);
                        }
                        index += 3;
                        continue;
                    }
                    if self.functions.contains(&name) && parenthesized(next) {
                        index += 2;
                        continue;
                    }
                    self.expected_binding(&name, dialect, bindings, seen, into);
                }
                TokenTree::Punct(_) => {}
            }
            index += 1;
        }
    }

    fn expected_binding(
        &self,
        name: &str,
        dialect: Option<Dialect>,
        bindings: &Bindings,
        seen: &mut HashSet<String>,
        into: &mut Vec<Expected>,
    ) {
        if let Some(value) = self.binding(name, bindings)
            && seen.insert(name.to_owned())
        {
            self.expected(value, dialect, bindings, seen, into);
        }
    }

    /// Reads one test: the strings each assertion that renders words expects, and whether one pins what a `.php` file
    /// reports.
    fn test(&self, name: String, body: &[TokenTree]) -> Test {
        let mut bindings = Bindings::new();
        self::bindings(body, &mut bindings);

        let mut expected = Vec::new();
        let mut asserts_php = false;
        for statement in statements(body) {
            if matches!(statement.first(), Some(TokenTree::Ident(keyword)) if keyword == "let") {
                continue;
            }
            let renders = self.renders(statement, &bindings);
            asserts_php |=
                (renders || asserts_nothing(statement)) && self.dialect(statement, &bindings) == Some(Dialect::Php);
            if renders {
                self.expected(statement, None, &bindings, &mut HashSet::new(), &mut expected);
            }
        }
        asserts_php |=
            expected.iter().any(|string| string.dialect == Some(Dialect::Php) && !message(&string.text).is_empty());

        Test { name, expected, asserts_php }
    }
}

/// Whether `statement` asserts that a result is empty, which pins every word it could hold.
fn asserts_nothing(statement: &[TokenTree]) -> bool {
    let [TokenTree::Ident(assert), bang, TokenTree::Group(arguments)] = statement else { return false };
    if !is_punct(Some(bang), '!') {
        return false;
    }
    let arguments = trees(arguments.stream());
    match assert.to_string().as_str() {
        "assert_eq" => split(&arguments, ',').iter().take(2).any(|argument| {
            let argument = compact(argument);
            ["[]", "vec![]", "Vec::new()"].contains(&argument.as_str())
                || (argument.starts_with("Vec::<") && argument.ends_with(">::new()"))
        }),
        "assert" => split(&arguments, ',').first().is_some_and(|condition| {
            let condition = compact(condition);
            !condition.starts_with('!') && condition.ends_with(".is_empty()")
        }),
        _ => false,
    }
}

/// `tokens` as source text without spaces.
fn compact(tokens: &[TokenTree]) -> String {
    tokens.iter().cloned().collect::<TokenStream>().to_string().replace(' ', "")
}

/// The words of a rendered issue: the string without its file, position, code and `format!` placeholders. Empty for a
/// string that holds no words, such as a file path or an issue code.
fn message(text: &str) -> String {
    let text = text.trim();
    if PATH.is_match(text) || CODE.is_match(text) {
        return String::new();
    }
    let filled = PLACEHOLDER.replace_all(text, "");
    let words = LEAD.replace(filled.trim(), "");
    if words.chars().any(char::is_alphabetic) { words.trim().to_owned() } else { String::new() }
}

/// The dialect of a string that names its own file, as an effect issue does.
fn own_dialect(text: &str) -> Option<Dialect> {
    LEAD.captures(text.trim()).and_then(|lead| lead.get(1)).map(|extension| Dialect::of_extension(extension.as_str()))
}

/// Every test of every message test file, named by its file.
fn message_tests() -> Vec<(String, Test)> {
    MESSAGE_TESTS
        .iter()
        .flat_map(|(path, source)| {
            let (file, tests) = TestFile::parse(source);
            tests.into_iter().map(move |(name, body)| (format!("{path}::{name}"), file.test(name, &body)))
        })
        .collect()
}

/// The `.sharp` messages a test asserts.
fn sharp_messages(test: &Test) -> Vec<String> {
    test.expected
        .iter()
        .filter(|string| own_dialect(&string.text).or(string.dialect) == Some(Dialect::Sharp))
        .map(|string| message(&string.text))
        .filter(|words| !words.is_empty())
        .collect()
}

#[test]
fn a_sharp_message_names_nothing_only_php_has() {
    let mut leaks = Vec::new();
    for (path, test) in message_tests() {
        for words in sharp_messages(&test) {
            let own_words = REFUSED_FORM.replace_all(&words, "");
            for (name, pattern) in BANNED.iter() {
                if let Some(found) = pattern.find(&own_words) {
                    leaks.push(format!("{path}: the `.sharp` message {words:?} names {name}: `{}`", found.as_str()));
                }
            }
        }
    }

    assert!(leaks.is_empty(), "A `.sharp` message speaks PHP#.\n{}", leaks.join("\n"));
}

#[test]
fn a_test_of_a_sharp_message_asserts_its_php_twin() {
    let allowed: HashSet<&str> = PHP_CANNOT_EXPRESS.iter().flat_map(|(_, tests)| tests.iter().copied()).collect();
    let mut failures = Vec::new();
    let mut used = HashSet::new();
    for (path, test) in message_tests() {
        if sharp_messages(&test).is_empty() || test.asserts_php {
            continue;
        }
        if allowed.contains(test.name.as_str()) {
            used.insert(test.name);
            continue;
        }
        failures.push(format!(
            "{path} asserts a `.sharp` message and no `.php` one: assert what the `.php` twin reports in the same \
             test, or list the test in `PHP_CANNOT_EXPRESS` under the reason PHP cannot write it"
        ));
    }
    for name in allowed.iter().filter(|name| !used.contains(**name)) {
        failures.push(format!(
            "`PHP_CANNOT_EXPRESS` lists {name}, which is no test of a `.sharp` message without a `.php` twin: remove it"
        ));
    }

    assert!(failures.is_empty(), "A `.sharp` rule leaves PHP alone.\n{}", failures.join("\n"));
}
