use std::path::Path;
use std::process::Command;
use std::process::Output;

fn fixture() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/structure"))
}

fn run(workspace: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mago"))
        .args(["--no-version-check", "--colors", "never", "structure"])
        .args(arguments)
        .env("MAGO_LOG", "off")
        .current_dir(workspace)
        .output()
        .unwrap()
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
