use mago_word::Word;

use crate::metadata::class_like::ClassLikeMetadata;
use crate::metadata::flags::MetadataFlags;
use crate::visibility::Visibility;

/// Inherits property declarations and appearances from a parent class-like.
/// Updates `declaring_property_ids`, `appearing_property_ids`, etc.
pub fn inherit_properties_from_parent(metadata: &mut ClassLikeMetadata, parent_metadata: &ClassLikeMetadata) {
    let classlike_name = metadata.name;
    let is_trait = metadata.kind.is_trait();
    let parent_is_trait = parent_metadata.kind.is_trait();

    // Magic `@property*` annotations document the inherited `__get`/`__set` interface, so they
    // apply to descendants as well; an own tag beats inherited ones.
    for (property_name, declaring_classlike) in &parent_metadata.magic_property_ids {
        metadata.magic_property_ids.entry(*property_name).or_insert(*declaring_classlike);
    }

    for (property_name, appearing_classlike) in &parent_metadata.appearing_property_ids {
        if metadata.has_appearing_property(*property_name) {
            continue;
        }

        if !parent_is_trait
            && let Some(parent_property_metadata) = parent_metadata.properties.get(property_name)
            && parent_property_metadata.is_final()
        {
            continue;
        }

        metadata
            .appearing_property_ids
            .insert(*property_name, if is_trait { classlike_name } else { *appearing_classlike });

        if parent_is_trait && let Some(parent_property) = parent_metadata.properties.get(property_name) {
            metadata.properties.entry(*property_name).or_insert_with(|| parent_property.clone());
        }
    }

    for (property_name, declaring_classlike) in &parent_metadata.declaring_property_ids {
        // Check if the parent property is final (private) before doing anything else.
        // Final properties cannot be inherited, so skip them entirely.
        if !parent_is_trait
            && let Some(parent_property_metadata) = parent_metadata.properties.get(property_name)
            && parent_property_metadata.is_final()
        {
            continue;
        }

        if metadata.declaring_property_ids.contains_key(property_name) {
            continue;
        }

        metadata.declaring_property_ids.insert(*property_name, *declaring_classlike);
    }

    for (property_name, inheritable_classlike) in &parent_metadata.inheritable_property_ids {
        let mut is_overridable = true;
        if !parent_is_trait {
            if let Some(parent_property_metadata) = parent_metadata.properties.get(property_name)
                && parent_property_metadata.is_final()
            {
                is_overridable = false;
            }

            if is_overridable {
                metadata.overridden_property_ids.entry(*property_name).or_default().insert(*inheritable_classlike);
                keep_parent_set(metadata, parent_metadata, *property_name);
            }
        }

        if is_overridable {
            metadata.inheritable_property_ids.insert(*property_name, *inheritable_classlike);
        }
    }
}

/// Spec section 6.1: a PHP# property's `set` reaches its namespace at most, but PHP refuses to narrow a parent's `set`,
/// so an override of a property whose `set` reaches further, a plain PHP parent's, keeps that `set`: its own `set`
/// without an access modifier is no longer `internal`.
fn keep_parent_set(metadata: &mut ClassLikeMetadata, parent_metadata: &ClassLikeMetadata, property_name: Word) {
    let Some(parent_property) = parent_metadata.properties.get(&property_name) else {
        return;
    };
    let parent_set_reaches_further = !parent_property.flags.is_internal_set()
        && !parent_property.flags.is_internal_reach()
        && parent_property.write_visibility != Visibility::Private;

    if parent_set_reaches_further && let Some(property) = metadata.properties.get_mut(&property_name) {
        property.flags.remove(MetadataFlags::INTERNAL_SET);
    }
}
