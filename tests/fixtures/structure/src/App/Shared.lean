import Sharp.Facts
open Sharp

namespace App.Shared

@[rule] def neverImportsTenant : Rule := .neverImports "App.Shared" "App.Tenant"

end App.Shared
