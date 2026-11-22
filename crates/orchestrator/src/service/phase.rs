//! The phases of a compile and an analysis, each of which logs how long it took at DEBUG, as "Phase scan took 0.12 s.".

#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;

/// One phase of a compile or an analysis, which logs how long it ran at DEBUG once [`Phase::end`] ends it.
///
/// A phase is `inputs`, `load`, `scan`, `populate`, `analysis`, `lowering`, `laws`, `translation`, `hosts`, `report` or
/// `write`. `translation` is the part of `laws` that translates each file the laws reach that kept its compiled file: a
/// file lowered again was translated in `lowering`. `hosts` is the extension hosts' own part: the compile of their folder
/// that runs before a run starts them, and the answers they give when they take an analysis over.
#[derive(Debug)]
#[must_use = "a phase logs its time only when it ends"]
pub struct Phase {
    name: &'static str,
    #[cfg(not(target_arch = "wasm32"))]
    started: Instant,
}

impl Phase {
    /// Starts the phase `name`.
    pub fn start(name: &'static str) -> Self {
        Self {
            name,
            #[cfg(not(target_arch = "wasm32"))]
            started: Instant::now(),
        }
    }

    /// Ends the phase, and logs at DEBUG how long it ran, as "Phase scan took 0.12 s.". WebAssembly reads no clock, so
    /// there it logs nothing.
    pub fn end(self) {
        #[cfg(not(target_arch = "wasm32"))]
        tracing::debug!("Phase {} took {:.2} s.", self.name, self.started.elapsed().as_secs_f64());
        #[cfg(target_arch = "wasm32")]
        let _ = self.name;
    }
}
