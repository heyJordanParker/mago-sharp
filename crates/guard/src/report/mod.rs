use mago_allocator::Arena;

use mago_collector::Collector;
use mago_database::file::File;
use mago_reporting::IssueCollection;

use crate::report::breach::BoundaryBreach;
use crate::report::flaw::StructuralFlaw;

pub mod breach;
pub mod flaw;

#[derive(Debug, Default)]
pub struct FortressReport {
    pub boundary_breaches: Vec<BoundaryBreach>,
    pub structural_flaws: Vec<StructuralFlaw>,
    /// Whether perimeter guard was skipped due to missing configuration.
    pub missing_perimeter_configuration: bool,
    /// Whether structural guard was skipped due to missing configuration.
    pub missing_structural_configuration: bool,
}

impl FortressReport {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.boundary_breaches.is_empty() && self.structural_flaws.is_empty()
    }

    pub fn report_into_issues<A>(self, arena: &A, source_file: &File) -> IssueCollection
    where
        A: Arena,
    {
        let mut collector = Collector::new(arena, source_file);
        for boundary_breach in self.boundary_breaches {
            collector.report_with_code(boundary_breach.vector.error_code(), boundary_breach.into());
        }

        for structural_flaw in self.structural_flaws {
            collector.report_with_code(structural_flaw.kind.error_code(), structural_flaw.into());
        }

        collector.finish()
    }
}
