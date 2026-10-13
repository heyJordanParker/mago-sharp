//! The PHP a PHP# interface runs as.
//!
//! Spec section 22 gives an interface method or computed property a default body. An interface with default bodies
//! runs as a PHP interface whose members are all abstract, and a PHP trait that holds the default bodies. The trait is
//! named after the interface, so `App\HasDesign` has the trait `App\HasDesign\Defaults`. Every PHP# class that
//! implements the interface uses the trait, and a plain PHP class writes `use HasDesign\Defaults;`.

/// The short name of the trait that holds an interface's default bodies, in a namespace named after the interface.
pub const DEFAULTS_TRAIT_SHORT_NAME: &str = "Defaults";
