use foldhash::HashMap;

use mago_word::Word;
use mago_word::word;

use crate::identifier::method::MethodIdentifier;
use crate::metadata::CodebaseMetadata;
use crate::metadata::class_like::ClassLikeMetadata;

/// Inherits method declarations and appearances from a parent class-like.
/// Updates `declaring_method_ids`, `appearing_method_ids`, etc.
pub fn inherit_methods_from_parent(
    metadata: &mut ClassLikeMetadata,
    parent_metadata: &ClassLikeMetadata,
    codebase: &CodebaseMetadata,
) {
    let class_like_name = metadata.name;
    let parent_is_trait = parent_metadata.kind.is_trait();

    // An adaptation's names are as written: the method is keyed as the trait keys its own, and the alias as the class
    // keys its own, spec section 24, so a plain class's `recordEntry as entry` meets a PHP# trait's `recordEntry`.
    let reverse_alias_map: Option<HashMap<Word, Vec<Word>>> = if parent_is_trait && !metadata.trait_alias_map.is_empty()
    {
        let mut map: HashMap<Word, Vec<Word>> = HashMap::default();
        for (original, alias) in metadata.get_trait_alias_map() {
            map.entry(parent_metadata.method_key(original.as_bytes()))
                .or_default()
                .push(metadata.method_key(alias.as_bytes()));
        }
        Some(map)
    } else {
        None
    };

    // A class-like with `exact_names` keys the method a plain PHP parent keys `dothing` by its declared name, `doThing`,
    // as the engine does, so only a method of that exact name overrides it.
    let translates = metadata.exact_names && !parent_metadata.exact_names;
    let child_key = |parent_key: Word, id: &MethodIdentifier| {
        if translates {
            codebase.function_likes.get(&id.get_key()).map_or(parent_key, |method| method.original_name)
        } else {
            parent_key
        }
    };

    for (method_name_lc, appearing_method_id) in &parent_metadata.appearing_method_ids {
        let key = child_key(*method_name_lc, appearing_method_id);
        let mut process_name = |aliased_method_name: Word| {
            if metadata.has_appearing_method(aliased_method_name) {
                return;
            }

            let implemented_method_id = MethodIdentifier::new(class_like_name, aliased_method_name);
            let final_appearing_id = if parent_is_trait { implemented_method_id } else { *appearing_method_id };
            metadata.appearing_method_ids.insert(aliased_method_name, final_appearing_id);
        };

        process_name(key);

        if let Some(reverse_map) = reverse_alias_map.as_ref()
            && let Some(aliases) = reverse_map.get(method_name_lc)
        {
            for alias in aliases.iter().copied() {
                process_name(alias);
            }
        }
    }

    for (method_name_lc, declaring_method_id) in &parent_metadata.inheritable_method_ids {
        let key = child_key(*method_name_lc, declaring_method_id);
        if !method_name_lc.eq(&word("__construct")) || parent_metadata.flags.has_consistent_constructor() {
            if parent_is_trait {
                // A PHP# class's own method replaces a trait's body, spec section 22, so it overrides it.
                if (metadata.flags.is_sharp() && metadata.methods.contains(&key))
                    || codebase
                        .function_likes
                        .get(&declaring_method_id.get_key())
                        .and_then(|meta| meta.method_metadata.as_ref())
                        .is_some_and(|method| method.is_abstract)
                {
                    metadata.add_overridden_method_parent(key, *declaring_method_id);
                }
            } else {
                metadata.add_overridden_method_parent(key, *declaring_method_id);
            }

            if let Some(existing_overridden) = metadata.overridden_method_ids.get_mut(&key)
                && let Some(parent_overridden_map) = parent_metadata.overridden_method_ids.get(method_name_lc)
            {
                existing_overridden.extend(parent_overridden_map.iter().map(|(k, v)| (*k, *v)));
            }
        }

        let process_name = |aliased_method_name: Word, metadata: &mut ClassLikeMetadata| {
            if let Some(implementing_method_id) = metadata.declaring_method_ids.get(&aliased_method_name) {
                let implementing_class = implementing_method_id.get_class_name();
                let implementing_method_name = implementing_method_id.get_method_name();

                let is_existing_pseudo_from_trait = !parent_is_trait
                    && codebase.class_likes.get(&implementing_class).is_some_and(|c| c.kind.is_trait())
                    && codebase
                        .function_likes
                        .get(&(implementing_class, implementing_method_name))
                        .is_some_and(|m| m.flags.is_magic_method());

                if !is_existing_pseudo_from_trait
                    && (!codebase
                        .method_is_abstract(implementing_class.as_bytes(), implementing_method_name.as_bytes())
                        || implementing_class == class_like_name)
                {
                    return;
                }
            }

            metadata.declaring_method_ids.insert(aliased_method_name, *declaring_method_id);

            let is_ctor_or_clone = aliased_method_name == word("__construct") || aliased_method_name == word("__clone");
            let is_inheritable = !parent_is_trait
                || metadata.kind.is_trait()
                || is_ctor_or_clone
                || metadata
                    .trait_visibility(aliased_method_name.as_bytes())
                    .or_else(|| {
                        codebase.get_method_visibility(parent_metadata.name.as_bytes(), aliased_method_name.as_bytes())
                    })
                    .is_none_or(|visibility| !visibility.is_private());

            if is_inheritable {
                metadata.inheritable_method_ids.insert(aliased_method_name, *declaring_method_id);
            }
        };

        process_name(key, metadata);

        if let Some(reverse_map) = reverse_alias_map.as_ref()
            && let Some(aliases) = reverse_map.get(method_name_lc)
        {
            for alias in aliases.iter().copied() {
                process_name(alias, metadata);
            }
        }
    }
}
