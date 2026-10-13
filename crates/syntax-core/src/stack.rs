//! Stack growth for the recursive passes over a syntax tree, as rustc's `rustc_data_structures::stack` does it.

/// The stack a guarded call needs left to run without growing it. It covers the deepest stack any code between two
/// guarded calls uses. rustc uses the same 100 KiB.
#[cfg(not(target_family = "wasm"))]
const RED_ZONE: usize = 100 * 1024;

/// The size of the stack segment the guard allocates when less than [`RED_ZONE`] is left. rustc uses the same 1 MiB.
#[cfg(not(target_family = "wasm"))]
const STACK_PER_RECURSION: usize = 1024 * 1024;

/// Runs `f`, on a new stack segment when less than [`RED_ZONE`] of the current stack is left.
///
/// Each recursive entry point of a pass over a syntax tree calls it, so a deeply nested tree never overflows the stack.
#[cfg(not(target_family = "wasm"))]
#[inline]
pub fn ensure_sufficient_stack<R>(f: impl FnOnce() -> R) -> R {
    stacker::maybe_grow(RED_ZONE, STACK_PER_RECURSION, f)
}

/// Runs `f`. `stacker` finds no stack limit on WebAssembly, so there it would allocate a new segment on every call.
#[cfg(target_family = "wasm")]
#[inline]
pub fn ensure_sufficient_stack<R>(f: impl FnOnce() -> R) -> R {
    f()
}
