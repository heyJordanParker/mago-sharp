use std::path::Path;
use std::process::Command;
use std::process::Output;

fn fixture() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/structure"))
}

fn command(workspace: &Path, arguments: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mago"));
    command
        .args(["--no-version-check", "--colors", "never", "structure"])
        .args(arguments)
        .env("MAGO_LOG", "off")
        .current_dir(workspace);
    command
}

fn run(workspace: &Path, arguments: &[&str]) -> Output {
    command(workspace, arguments).output().unwrap()
}

fn copy_fixture() -> tempfile::TempDir {
    let workspace = tempfile::tempdir().unwrap();
    for file in ["mago.toml", "src/App/Shared.lean", "src/App/Shared/Money.php", "src/App/Tenant/Store.php"] {
        let target = workspace.path().join(file);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::copy(fixture().join(file), target).unwrap();
    }

    workspace
}

#[test]
fn passes_when_every_rule_holds() {
    let workspace = copy_fixture();

    let output = run(workspace.path(), &["--reporting-format", "rich"]);
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    assert!(workspace.path().join(".mago/structure/App/Shared.olean").is_file());
}

#[test]
fn builds_sharp_rules_in_the_mago_cache_folder_and_deletes_old_keys() {
    let workspace = copy_fixture();
    let cache = tempfile::tempdir().unwrap();
    let packages = cache.path().join("mago/structure");
    std::fs::create_dir_all(packages.join("0123456789abcdef")).unwrap();

    let output = command(workspace.path(), &["--reporting-format", "rich"])
        .env("XDG_CACHE_HOME", cache.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let keys: Vec<_> = std::fs::read_dir(&packages).unwrap().map(|entry| entry.unwrap().path()).collect();
    assert_eq!(keys.len(), 1, "{keys:?}");
    assert_ne!(keys[0], packages.join("0123456789abcdef"));
    assert!(keys[0].join(".lake/build/bin/sharp-rules").is_file(), "{keys:?}");
}

#[test]
fn checks_a_rule_over_thousands_of_imports() {
    let workspace = copy_fixture();
    let imports: String = (0..4072).map(|index| format!("use Vendor\\Package\\Class{index};\n")).collect();
    std::fs::write(
        workspace.path().join("src/App/Shared/Imports.php"),
        format!("<?php\n\nnamespace App\\Shared;\n\n{imports}"),
    )
    .unwrap();

    let output = run(workspace.path(), &["--reporting-format", "rich"]);
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn fails_on_the_php_line_that_breaks_a_rule() {
    let workspace = copy_fixture();
    let money = workspace.path().join("src/App/Shared/Money.php");
    let code = std::fs::read_to_string(&money).unwrap();
    std::fs::write(
        &money,
        code.replace("namespace App\\Shared;\n", "namespace App\\Shared;\n\nuse App\\Tenant\\Store;\n"),
    )
    .unwrap();

    let output = run(workspace.path(), &["--reporting-format", "rich"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}{}", String::from_utf8_lossy(&output.stderr));
    assert!(stdout.contains("src/App/Shared/Money.php:5:5"), "{stdout}");
    assert!(stdout.contains("App.Shared.neverImportsTenant"), "{stdout}");
    assert!(stdout.contains("imports App.Tenant.Store"), "{stdout}");
}

#[test]
fn checks_the_rule_form_the_specification_writes() {
    let workspace = copy_fixture();
    std::fs::create_dir_all(workspace.path().join("src/App/Tenant/Store")).unwrap();
    std::fs::write(
        workspace.path().join("src/App/Tenant/Store.lean"),
        "import Sharp.Facts\nopen Sharp\n\nnamespace App.Tenant.Store\n\n@[rule] def requires : Rule :=\n  Sharp.importsOf \"App.Tenant.Store\" ⊆ [\"App.Tenant.Community\", \"App.Shared.Schema\"]\n\nend App.Tenant.Store\n",
    )
    .unwrap();
    std::fs::write(
        workspace.path().join("src/App/Tenant/Store/Refunds.php"),
        "<?php\n\nnamespace App\\Tenant\\Store;\n\nuse App\\Shared\\Money;\nuse App\\Tenant\\Community\\Member;\nuse App\\Tenant\\Store\\Receipt;\n\nfinal class Refunds {}\n",
    )
    .unwrap();

    let output = run(workspace.path(), &["--reporting-format", "rich"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}{}", String::from_utf8_lossy(&output.stderr));
    assert!(stdout.contains("src/App/Tenant/Store/Refunds.php:5:5"), "{stdout}");
    assert!(stdout.contains("App.Tenant.Store.requires"), "{stdout}");
    assert!(stdout.contains("imports App.Shared.Money"), "{stdout}");
    assert!(!stdout.contains("Refunds.php:6"), "{stdout}");
    assert!(!stdout.contains("Refunds.php:7"), "{stdout}");
}

#[test]
fn reports_a_rules_file_lean_rejects() {
    let workspace = copy_fixture();
    std::fs::write(
        workspace.path().join("src/App/Shared.lean"),
        "import Sharp.Facts\nopen Sharp\n@[rule] def broken : Nat := 1\n",
    )
    .unwrap();

    let output = run(workspace.path(), &["--reporting-format", "rich"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}{}", String::from_utf8_lossy(&output.stderr));
    assert!(stdout.contains("unproven-rule"), "{stdout}");
    assert!(stdout.contains("`@[rule]` needs a definition of type `Sharp.Rule`"), "{stdout}");
}
