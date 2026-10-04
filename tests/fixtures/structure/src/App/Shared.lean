import Sharp
namespace App.Shared
theorem neverImportsTenant : ∀ i ∈ Sharp.imports, Sharp.under i.namespace_ "App.Shared" → ¬ Sharp.under i.name "App.Tenant" := by sharp_decide
end App.Shared
