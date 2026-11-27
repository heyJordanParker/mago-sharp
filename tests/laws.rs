//! Runs `mago compile` and `mago analyze` on projects whose laws the law checker proves or refuses. A law's outcome is
//! Lean's kernel judging a theorem under a deterministic heartbeat budget, which reads nothing that differs by
//! platform, so these tests are a test binary of their own, which Linux's MAGO slices and the Mac's job both run. The
//! tests of Lean's install, its runner, its process handling and its paths, which can fail on one platform alone, are
//! in `tests/compile.rs`.

mod common;

use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use mago_database::process;
use mago_sharp_lean::PACKAGE_FOLDER;

use common::compiling::LAWFUL_MONEY;
use common::compiling::SHOP_MODULE;
use common::compiling::analyze;
#[cfg(unix)]
use common::compiling::cache_with_runner;
use common::compiling::command;
use common::compiling::compile;
use common::compiling::compile_lines;
use common::compiling::compile_succeeds;
use common::compiling::compile_without_lean;
use common::compiling::compiled;
#[cfg(unix)]
use common::compiling::counting_cache;
use common::compiling::files_under;
use common::compiling::issue_lines;
use common::compiling::lawful_project;
use common::compiling::library_project;
use common::compiling::money_and_wallet;
use common::compiling::printed;
use common::compiling::source_lean_files;
use common::compiling::write;

#[test]
fn compile_proves_a_law_from_its_sharp_file_alone_and_writes_the_compiled_file() {
    let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert_eq!(source_lean_files(root), Vec::<PathBuf>::new());
    assert!(compiled(root, "app/Shared/Money.sharp").exists());
    assert!(root.join(".sharp").join(PACKAGE_FOLDER).is_dir(), "the compile keeps the Lake package's folder");
    assert!(!root.join(".sharp/.lean/Code").exists(), "the runner's request holds the generated code, so no file does");
}

#[test]
fn compile_proves_a_law_in_a_project_that_installs_the_standard_library() {
    let directory = lawful_project(&[
        ("app/Shared/Money.sharp", LAWFUL_MONEY),
        (
            "vendor/heyjordanparker/php-sharp-composer/composer.json",
            "{\n    \"name\": \"heyjordanparker/php-sharp-composer\",\n    \"autoload\": {\n        \"psr-4\": {\n            \"Sharp\\\\\": \"library/Sharp/\"\n        }\n    }\n}\n",
        ),
        (
            "vendor/heyjordanparker/php-sharp-composer/library/Sharp/Math.sharp",
            "namespace Sharp;\n\npublic static class Math\n{\n    public static int twice(int value) => value * 2;\n}\n",
        ),
    ]);
    let root = directory.path();

    let output = compile_lines(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shared/Money.sharp").exists());
}

#[test]
fn compile_refuses_a_law_a_changed_method_breaks_on_the_law_line_and_writes_no_compiled_file() {
    let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]);
    let root = directory.path();
    compile_succeeds(root);

    write(
        root,
        "app/Shared/Money.sharp",
        &LAWFUL_MONEY
            .replace("this.amount + other.amount, this.currency", "this.amount + other.amount, other.currency"),
    );
    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "app/Shared/Money.sharp:8:5: error[law-unproven]: law addKeepsCurrency is not proven\n = still to show: new Money(a.amount + b.amount, b.currency).currency == a.currency\n"
        ),
        "{}",
        printed(&output)
    );
    assert!(!compiled(root, "app/Shared/Money.sharp").exists());
    assert_eq!(source_lean_files(root), Vec::<PathBuf>::new());
}

/// Spec section 28: a law reaches the body of an operator it applies as it reaches a called method's, so an edit of
/// the operator's body proves again a law in a file that only applies it, and refuses the law the edit breaks.
#[test]
fn compile_refuses_a_law_an_edit_of_an_operator_it_applies_breaks() {
    let money = "namespace App.Shared;\n\npublic final class Money\n{\n    public Money(public int amount { get; }) { }\n\n    public static int operator <=>(Money a, Money b) => a.amount <=> b.amount;\n}\n";
    let till = "namespace App.Shared;\n\npublic final class Till\n{\n    law ordersByAmount(int low, int high) when low < high => new Money(low) < new Money(high);\n}\n";
    let directory = lawful_project(&[("app/Shared/Money.sharp", money), ("app/Shared/Till.sharp", till)]);
    let root = directory.path();
    compile_succeeds(root);
    assert!(compiled(root, "app/Shared/Till.sharp").exists());

    write(root, "app/Shared/Money.sharp", &money.replace("a.amount <=> b.amount", "b.amount <=> a.amount"));
    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains("app/Shared/Till.sharp:5:5: error[law-unproven]: law ordersByAmount is not proven\n"),
        "{}",
        printed(&output)
    );
    assert!(!compiled(root, "app/Shared/Till.sharp").exists());
}

/// Spec sections 19 and 28: `==` on two Lists runs the elements' `operator ==`, so a law reaches it as it reaches an
/// operator it applies, and an edit of that operator's body proves again a law in a file that only compares Lists.
#[test]
fn compile_refuses_a_law_an_edit_of_the_operator_its_lists_compare_by_breaks() {
    let shape = "namespace App.Shared;\n\npublic final class Shape\n{\n    public Shape(public int amount { get; }) { }\n\n    public static bool operator ==(Shape a, Shape b) => a.amount == b.amount;\n\n    public int hash() => this.amount;\n}\n";
    let till = "namespace App.Shared;\n\npublic final class Till\n{\n    law sameListSameAmount(Shape a, Shape b) => !([a] == [b]) || a.amount == b.amount;\n}\n";
    let directory = lawful_project(&[("app/Shared/Shape.sharp", shape), ("app/Shared/Till.sharp", till)]);
    let root = directory.path();
    compile_succeeds(root);
    assert!(compiled(root, "app/Shared/Till.sharp").exists());

    write(root, "app/Shared/Shape.sharp", &shape.replace("=> a.amount == b.amount;", "=> true;"));
    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output)
            .contains("app/Shared/Till.sharp:5:5: error[law-unproven]: law sameListSameAmount is not proven\n"),
        "{}",
        printed(&output)
    );
    assert!(!compiled(root, "app/Shared/Till.sharp").exists());
}

/// Spec section 10: a struct states a law as a class does. Mago proves it, and refuses it once a changed method breaks
/// it.
#[test]
fn a_struct_states_a_law() {
    let money = LAWFUL_MONEY.replace("public class Money", "public struct Money");
    let directory = lawful_project(&[("app/Shared/Money.sharp", &money)]);
    let root = directory.path();
    compile_succeeds(root);

    write(
        root,
        "app/Shared/Money.sharp",
        &money.replace("this.amount + other.amount, this.currency", "this.amount + other.amount, other.currency"),
    );
    let output = compile_lines(root);

    assert!(
        printed(&output)
            .contains("app/Shared/Money.sharp:8:5:error - law-unproven: law addKeepsCurrency is not proven"),
        "{}",
        printed(&output)
    );
}

#[test]
fn compile_proves_two_stale_law_files_and_refuses_each_one_whose_method_breaks_its_law() {
    let directory = money_and_wallet();
    let root = directory.path();
    compile_succeeds(root);
    assert!(compiled(root, "app/Shared/Wallet.sharp").exists());

    for class in ["Money", "Wallet"] {
        write(
            root,
            &format!("app/Shared/{class}.sharp"),
            &LAWFUL_MONEY
                .replace("this.amount + other.amount, this.currency", "this.amount + other.amount, other.currency")
                .replace("Money", class),
        );
    }
    let output = compile_lines(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    for class in ["Money", "Wallet"] {
        assert!(
            printed(&output).contains(&format!(
                "app/Shared/{class}.sharp:8:5:error - law-unproven: law addKeepsCurrency is not proven"
            )),
            "{class}: {}",
            printed(&output)
        );
        assert!(!compiled(root, &format!("app/Shared/{class}.sharp")).exists(), "{class}");
    }
}

/// What a runner's request holds.
#[cfg(unix)]
struct RunnerRequest {
    /// Each generated module's name and source.
    modules: Vec<(String, String)>,
    /// Each theorem's statement, text, fixed steps and its law's search for the small values that break it, each the
    /// empty string when it has none.
    theorems: Vec<(String, String, String, String)>,
}

/// The request of the one runner `counting_cache` started in `folder`: its header and the threads and heartbeats it
/// answers with, the modules an earlier runner stopped in, the generated modules, then each check's modules and
/// theorems.
#[cfg(unix)]
fn runner_request(folder: &Path) -> RunnerRequest {
    let mut started = std::fs::read_dir(folder.join("started")).unwrap();
    let request = std::fs::read(started.next().unwrap().unwrap().path()).unwrap();
    assert!(started.next().is_none(), "one runner started");
    let mut at = 20;
    let length = |at: &mut usize| {
        let length = u32::from_be_bytes(request[*at..*at + 4].try_into().unwrap());
        *at += 4;
        usize::try_from(length).unwrap()
    };
    let string = |at: &mut usize| {
        let bytes = length(at);
        *at += bytes;
        String::from_utf8(request[*at - bytes..*at].to_vec()).unwrap()
    };
    for _ in 0..length(&mut at) {
        string(&mut at);
    }
    let modules = (0..length(&mut at)).map(|_| (string(&mut at), string(&mut at))).collect();
    let mut theorems = Vec::new();
    for _ in 0..length(&mut at) {
        for _ in 0..length(&mut at) {
            string(&mut at);
        }
        for _ in 0..length(&mut at) {
            let (statement, text, steps) = (string(&mut at), string(&mut at), string(&mut at));
            at += 1;
            theorems.push((statement, text, steps, string(&mut at)));
        }
    }
    assert_eq!(at, request.len(), "the request ends after its checks");

    RunnerRequest { modules, theorems }
}

/// The generated Lean module of `name` in the request of the one runner `counting_cache` started in `folder`.
#[cfg(unix)]
fn generated_module(folder: &Path, name: &str) -> String {
    runner_request(folder)
        .modules
        .into_iter()
        .find(|(module, _)| module == name)
        .map(|(_, text)| text)
        .expect("the request holds the module")
}

/// Spec section 28: a law with a body has fixed steps too, which close each of its cases as the fixed steps close a law
/// without one, and the law checker tries them before Lean's search. Each of Money's laws with a body, a recursion on an
/// `int` and one on a list, gets them, and Money proves.
#[cfg(unix)]
#[test]
fn a_law_with_a_body_has_fixed_steps_for_each_of_its_cases() {
    let directory = library_project(&[("app/Shared/Money.sharp", SPECIMEN_MONEY)]);
    let root = directory.path();
    let lake = tempfile::tempdir().unwrap();

    let output = process::output(command(root, "never").env("XDG_CACHE_HOME", counting_cache(lake.path()))).unwrap();

    assert!(output.status.success(), "{}", printed(&output));
    let theorems = runner_request(lake.path()).theorems;
    for statement in ["«App».«Shared».«Money».«timesKeepsCurrency»", "«App».«Shared».«Money».«sumKeepsCurrency»"]
    {
        let (_, text, steps, _) =
            theorems.iter().find(|(written, ..)| written == statement).expect("the law's theorem");
        assert_eq!(text.matches("sharp_close").count(), 2, "{text}");
        assert!(!steps.is_empty() && !steps.contains("sharp_close"), "{statement}: {steps}");
    }
}

/// Spec section 28: a law's search for the small values that break it travels with the law's theorem, and no generated
/// module holds it, so a law whose proof closes never elaborates or compiles its search.
#[cfg(unix)]
#[test]
fn a_law_s_search_travels_with_its_theorem_and_no_generated_module_holds_it() {
    let directory = library_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]);
    let root = directory.path();
    let lake = tempfile::tempdir().unwrap();

    let output = process::output(command(root, "never").env("XDG_CACHE_HOME", counting_cache(lake.path()))).unwrap();

    assert!(output.status.success(), "{}", printed(&output));
    let request = runner_request(lake.path());
    for (module, text) in &request.modules {
        assert!(!text.contains("«$breaking»") && !text.contains("«$claim»"), "{module}: {text}");
    }
    let (_, _, _, search) = request
        .theorems
        .iter()
        .find(|(statement, ..)| statement == "«App».«Shared».«Money».«addKeepsCurrency»")
        .expect("the law's theorem");
    assert!(search.contains("protected def «App».«Shared».«Money».«addKeepsCurrency».«$breaking»"), "{search}");
}

/// A law the fixed steps prove ends in one runner, which imports only the runtime library. A law they leave unproven
/// goes on to a second runner, with Lean's search, which ends its proof as it always did: here, with the case Lean's
/// search leaves open.
#[cfg(unix)]
#[test]
fn a_law_the_fixed_steps_leave_unproven_goes_on_to_lean_s_search() {
    let purse = LAWFUL_MONEY
        .replace("Money", "Purse")
        .replace("this.amount + other.amount, this.currency", "this.amount + other.amount, other.currency");
    let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY), ("app/Shared/Purse.sharp", &purse)]);
    let root = directory.path();
    let lake = tempfile::tempdir().unwrap();

    let output = process::output(
        command(root, "never").env("MAGO_REPORTING_FORMAT", "emacs").env("XDG_CACHE_HOME", counting_cache(lake.path())),
    )
    .unwrap();

    let names = |request: &[u8], text: &str| request.windows(text.len()).any(|window| window == text.as_bytes());
    let mut requests: Vec<Vec<u8>> = std::fs::read_dir(lake.path().join("started"))
        .unwrap()
        .map(|entry| std::fs::read(entry.unwrap().path()).unwrap())
        .collect();
    requests.sort_by_key(|request| names(request, "Sharp.Lean.Proof"));
    assert_eq!(requests.len(), 2, "two runners started");
    let [steps, search] = [&requests[0], &requests[1]];
    assert!(!names(steps, "Sharp.Lean.Proof"), "the first runner imports only the runtime library");
    assert!(
        names(steps, "«App».«Shared».«Money».«addKeepsCurrency».proof")
            && names(steps, "«App».«Shared».«Purse».«addKeepsCurrency».proof")
    );
    assert!(names(search, "Sharp.Lean.Proof"), "the second runner proves with Lean's search");
    assert!(!names(search, "«App».«Shared».«Money».«addKeepsCurrency».proof"), "the proven law is not proved again");
    assert!(names(search, "«App».«Shared».«Purse».«addKeepsCurrency».proof"));
    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output)
            .contains("app/Shared/Purse.sharp:8:5:error - law-unproven: law addKeepsCurrency is not proven"),
        "{}",
        printed(&output)
    );
    assert!(compiled(root, "app/Shared/Money.sharp").exists());
    assert!(!compiled(root, "app/Shared/Purse.sharp").exists());
}

#[test]
fn compile_refuses_an_unproven_law_every_time_and_never_leaves_a_current_compiled_file() {
    let broken =
        LAWFUL_MONEY.replace("this.amount + other.amount, this.currency", "this.amount + other.amount, other.currency");
    let directory = lawful_project(&[("app/Shared/Money.sharp", &broken)]);
    let root = directory.path();

    for run in ["first", "second"] {
        let output = compile_lines(root);

        assert_eq!(output.status.code(), Some(1), "{run}: {}", printed(&output));
        assert!(
            printed(&output)
                .contains("app/Shared/Money.sharp:8:5:error - law-unproven: law addKeepsCurrency is not proven"),
            "{run}: {}",
            printed(&output)
        );
        assert!(!compiled(root, "app/Shared/Money.sharp").exists(), "{run}");
    }
}

/// A law's proof ends once while its key holds, passed or failed: the next compile reports a failed proof from
/// `.sharp/proofs.json`, so it needs no Lean, and an edit no law reaches proves nothing again.
#[test]
fn a_proof_is_not_attempted_again_while_its_law_is_unchanged() {
    let broken =
        LAWFUL_MONEY.replace("this.amount + other.amount, this.currency", "this.amount + other.amount, other.currency");
    let directory = lawful_project(&[("app/Shared/Money.sharp", &broken)]);
    let root = directory.path();
    let unproven = "app/Shared/Money.sharp:8:5:error - law-unproven: law addKeepsCurrency is not proven";
    let first = compile_lines(root);
    assert!(printed(&first).contains(unproven), "{}", printed(&first));

    let output = compile_without_lean(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(printed(&output).contains(unproven), "the failed proof is reported again: {}", printed(&output));
    assert!(!printed(&output).contains("law-checker-unavailable"), "{}", printed(&output));

    write(root, "app/Shared/Money.sharp", LAWFUL_MONEY);
    compile_succeeds(root);
    let unread = "    public int pages() => 1;\n\n    law addKeepsCurrency";
    write(root, "app/Shared/Money.sharp", &LAWFUL_MONEY.replace("    law addKeepsCurrency", unread));
    let output = compile_without_lean(root);

    assert!(output.status.success(), "the proof ran again: {}", printed(&output));
    assert!(compiled(root, "app/Shared/Money.sharp").exists());
}

/// A law's outcome depends on its texts, the law checker's library, its Lean version and the prover's settings, and
/// on nothing else of Mago's build: `.sharp/proofs.json` names no build, so a compile by another build of Mago, as a
/// release that changes only its version, proves no law again and needs no Lean. The project has no `vendor` folder,
/// whose warning keeps the compile from recording its inputs, so the next compile checks every law again.
#[test]
fn a_compile_by_another_build_of_mago_proves_no_law_again() {
    let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]);
    let root = directory.path();
    compile_succeeds(root);
    let proofs: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join(".sharp/proofs.json")).unwrap()).unwrap();
    assert_eq!(proofs.as_object().unwrap().keys().collect::<Vec<_>>(), ["proofs"]);

    let output = compile_without_lean(root);

    assert!(output.status.success(), "a law was proved again: {}", printed(&output));
}

/// An edit of a declaration one law reaches proves that law again, and no other: the other file's law is not checked.
#[test]
fn an_edit_of_a_declaration_one_law_reaches_proves_that_law_alone() {
    let label = "namespace App.Shared;\n\npublic class Label\n{\n    public Label(public string text { get; }) { }\n    public Label same() => new Label(this.text);\n\n    law sameKeepsText(Label l) => l.same().text == l.text;\n}\n";
    let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY), ("app/Shared/Label.sharp", label)]);
    let root = directory.path();
    compile_succeeds(root);
    let swapped = "new Money(other.amount + this.amount, this.currency)";
    write(
        root,
        "app/Shared/Money.sharp",
        &LAWFUL_MONEY.replace("new Money(this.amount + other.amount, this.currency)", swapped),
    );

    let output = process::output(command(root, "never").env("MAGO_LOG", "debug")).unwrap();

    assert!(output.status.success(), "{}", printed(&output));
    let counted = "0 laws keep the outcome of their proof, and 1 laws are proved.";
    assert!(printed(&output).contains(counted), "{}", printed(&output));
}

/// With `MAGO_LOG=debug`, the laws phase of a compile logs how long each of its parts took: finding the laws' proof keys,
/// finding or building the law checker's library, writing the Lean package, and each runner, then how each law's proof
/// ended and its time and heartbeats in the provers, the most heartbeats first.
#[test]
fn a_compile_logs_each_part_of_the_laws_phase_and_each_law_s_heartbeats_most_first_at_the_debug_level() {
    let label = "namespace App.Shared;\n\npublic class Label\n{\n    public Label(public string text { get; }) { }\n    public Label same() => new Label(this.text);\n\n    law sameKeepsText(Label l) => l.same().text == l.text;\n}\n";
    let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY), ("app/Shared/Label.sharp", label)]);

    let output = process::output(command(directory.path(), "never").env("MAGO_LOG", "debug")).unwrap();

    let printed = printed(&output);
    assert!(output.status.success(), "{printed}");
    let parts = [
        "Found the proof keys of 2 laws in ",
        "The law checker's library took ",
        "Wrote the Lean package of ",
        "A runner proved 2 laws with ",
    ];
    for part in parts {
        assert!(printed.contains(part), "no line holds {part:?}: {printed}");
    }
    let laws: Vec<(&str, u64)> = printed
        .lines()
        .filter_map(|line| line.split_once("The law ")?.1.split_once(" thousand heartbeats of the provers' time."))
        .filter_map(|(law, _)| {
            let (name, spent) = law.split_once(" ended proved after ")?;
            Some((name, spent.split_once(" s and ")?.1.parse().ok()?))
        })
        .collect();
    let mut names: Vec<&str> = laws.iter().map(|(name, _)| *name).collect();
    names.sort_unstable();
    assert_eq!(names, ["App.Shared.Label.sameKeepsText", "App.Shared.Money.addKeepsCurrency"], "{printed}");
    assert!(laws.iter().all(|(_, heartbeats)| *heartbeats > 0), "a law counted no heartbeats: {laws:?}");
    assert!(laws.windows(2).all(|pair| pair[0].1 >= pair[1].1), "the laws are not most heartbeats first: {laws:?}");
}

/// `.sharp/proofs.json` keeps how Lean judged a law, and never that Lean rejected a generated module the law reaches,
/// since an edit the law's key does not hold can fix the module: here a runner whose request opens no `Sharp` rejects
/// every module, and the next compile, after an edit no law reaches, checks the laws again and proves them.
#[cfg(unix)]
#[test]
fn a_law_whose_module_lean_rejected_is_checked_again_by_the_next_compile() {
    let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]);
    let root = directory.path();
    let lake = tempfile::tempdir().unwrap();
    let rejecting = cache_with_runner(lake.path(), "perl -pe 's/^open Sharp$/open Sharq/' | \"$real\" \"$@\"");
    let refused = process::output(command(root, "never").env("XDG_CACHE_HOME", rejecting)).unwrap();
    assert!(printed(&refused).contains("the checker translated the code it reaches wrongly"), "{}", printed(&refused));
    let unread = "    public int pages() => 1;\n\n    law addKeepsCurrency";
    write(root, "app/Shared/Money.sharp", &LAWFUL_MONEY.replace("    law addKeepsCurrency", unread));

    let output = compile(root);

    assert!(output.status.success(), "the laws were checked again: {}", printed(&output));
    assert!(compiled(root, "app/Shared/Money.sharp").exists());
}

/// A law Lean judged unproven is an error of its program, which `.sharp/proofs.json` keeps: the compile keeps its record
/// too, so a compile with every input as it was refuses the law again from the record, without compiling.
#[test]
fn a_compile_after_one_that_left_a_law_unproven_with_nothing_changed_refuses_it_again_without_compiling() {
    let broken =
        LAWFUL_MONEY.replace("this.amount + other.amount, this.currency", "this.amount + other.amount, other.currency");
    let directory = library_project(&[("app/Shared/Money.sharp", &broken)]);
    let root = directory.path();
    let refused = compile_lines(root);
    let unproven = "app/Shared/Money.sharp:8:5:error - law-unproven: law addKeepsCurrency is not proven";
    assert!(printed(&refused).contains(unproven), "{}", printed(&refused));
    let recorded = || std::fs::metadata(root.join(".sharp/inputs.json")).map(|record| record.modified().unwrap());
    let record = recorded().expect("the compile kept its record");

    let again = compile_lines(root);

    assert_eq!(again.status.code(), Some(1), "{}", printed(&again));
    assert_eq!(again.stdout, refused.stdout);
    assert_eq!(recorded().ok(), Some(record), "the compile kept the record and compiled nothing");
}

/// A law checker that couldn't be installed, and a generated module Lean rejected, depend on what no record holds, so a
/// compile that ends a law either way keeps no record: the next compile, with nothing changed, checks the law again and
/// proves it.
#[cfg(unix)]
#[test]
fn a_compile_after_one_that_could_not_judge_a_law_checks_it_again_with_nothing_changed() {
    let checks_again = |first: &dyn Fn(&Path) -> std::process::Output, why: &str| {
        let directory = library_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]);
        let root = directory.path();
        let refused = first(root);
        assert!(printed(&refused).contains(why), "{why}: {}", printed(&refused));

        let output = compile(root);

        assert!(output.status.success(), "{why}: the law was not checked again: {}", printed(&output));
        assert!(compiled(root, "app/Shared/Money.sharp").exists(), "{why}");
    };
    let lake = tempfile::tempdir().unwrap();
    let rejecting = cache_with_runner(lake.path(), "perl -pe 's/^open Sharp$/open Sharq/' | \"$real\" \"$@\"");

    checks_again(
        &|root| process::output(command(root, "never").env("XDG_CACHE_HOME", &rejecting)).unwrap(),
        "the checker translated the code it reaches wrongly",
    );
    checks_again(&compile_without_lean, "law-checker-unavailable");
}

#[test]
fn compile_proves_a_law_an_edit_adds_from_the_sharp_file_alone() {
    let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]);
    let root = directory.path();
    compile_succeeds(root);

    write(
        root,
        "app/Shared/Money.sharp",
        &LAWFUL_MONEY.replace(
            "a.add(b).currency == a.currency;\n",
            "a.add(b).currency == a.currency;\n    law addKeepsOtherCurrency(Money a, Money b) => a.add(b).currency == a.currency;\n",
        ),
    );
    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shared/Money.sharp").exists());
    assert_eq!(source_lean_files(root), Vec::<PathBuf>::new());
}

#[cfg(unix)]
#[test]
fn compile_proves_a_law_whose_calls_go_from_one_file_to_another_and_back_with_no_import_cycle() {
    let directory = lawful_project(&[
        (
            "app/Shared/Alpha.sharp",
            "namespace App.Shared;\n\npublic class Alpha\n{\n    public static int start(int a) => Beta.middle(a);\n    public static int finish(int a) => a;\n\n    law roundTrip(int a) => Alpha.start(a) == a;\n}\n",
        ),
        (
            "app/Shared/Beta.sharp",
            "namespace App.Shared;\n\npublic class Beta\n{\n    public static int middle(int a) => Alpha.finish(a);\n}\n",
        ),
    ]);
    let root = directory.path();
    let lake = tempfile::tempdir().unwrap();

    let output = process::output(command(root, "never").env("XDG_CACHE_HOME", counting_cache(lake.path()))).unwrap();

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shared/Alpha.sharp").exists());
    let alpha = generated_module(lake.path(), "Code.«App».«Shared».«Alpha»");
    assert!(
        alpha.contains("def «App».«Shared».«Beta».«middle»"),
        "the group's module holds Beta's declaration: {alpha}"
    );
    assert!(!alpha.contains("import Code."), "{alpha}");
}

/// A translated class derives the instances its proofs use and no `Repr`, which only prints a value, so the runner
/// spends nothing deriving one.
#[cfg(unix)]
#[test]
fn a_generated_module_derives_no_repr_since_no_proof_prints_a_value() {
    let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]);
    let root = directory.path();
    let lake = tempfile::tempdir().unwrap();

    let output = process::output(command(root, "never").env("XDG_CACHE_HOME", counting_cache(lake.path()))).unwrap();

    assert!(output.status.success(), "{}", printed(&output));
    let money = generated_module(lake.path(), "Code.«App».«Shared».«Money»");
    assert!(money.contains("structure «App».«Shared».«Money» where"), "{money}");
    assert!(money.contains("  deriving DecidableEq, Inhabited\n"), "{money}");
    assert!(!money.contains("Repr"), "{money}");
}

/// A call through the class, as `ShapeMath.doubled(s)`, is that static call already, as in C#, so it compiles and its
/// law is proved the same way.
#[cfg(unix)]
#[test]
fn compile_proves_a_law_through_an_extension_member_as_the_static_call_it_runs_as() {
    let directory = lawful_project(&[
        (
            "app/Shared/Shape.sharp",
            "namespace App.Shared;\n\nimport App.Shared.ShapeMath;\n\npublic class Shape\n{\n    public Shape(public int sides { get; }) { }\n\n    public static int doubledOr(Shape? s) => s?.doubled() ?? 0;\n    public static bool flatOr(Shape? s) => s?.isFlat ?? true;\n    public static int direct(Shape s) => ShapeMath.doubled(s) + (ShapeMath.isFlat(s) ? 1 : 0);\n\n    law doubles(Shape s) => s.doubled() == s.sides + s.sides;\n    law flat(Shape s) => s.isFlat == (s.sides < 3);\n    law twice(int n) => n.twice() == n + n;\n    law absent(int unused) => Shape.doubledOr(null) == 0 && Shape.flatOr(null);\n    law throughTheClass(Shape s) => ShapeMath.doubled(s) == s.doubled() && ShapeMath.isFlat(s) == s.isFlat;\n}\n",
        ),
        (
            "app/Shared/ShapeMath.sharp",
            "namespace App.Shared;\n\npublic static class ShapeMath\n{\n    extension(Shape shape)\n    {\n        public int doubled() => shape.sides + shape.sides;\n        public bool isFlat => shape.sides < 3;\n    }\n\n    extension(int n)\n    {\n        public int twice() => n + n;\n    }\n}\n",
        ),
    ]);
    let root = directory.path();
    let lake = tempfile::tempdir().unwrap();

    let output = process::output(command(root, "never").env("XDG_CACHE_HOME", counting_cache(lake.path()))).unwrap();

    assert!(output.status.success(), "{}", printed(&output));
    let laws: String = runner_request(lake.path()).modules.into_iter().map(|(_, text)| text).collect();
    for call in [
        "«App».«Shared».«ShapeMath».«doubled» «s»",
        "«App».«Shared».«ShapeMath».«isFlat» «s»",
        "«App».«Shared».«ShapeMath».«twice» «n»",
        "«App».«Shared».«ShapeMath».«doubled» («s».getD default)",
        "«App».«Shared».«ShapeMath».«isFlat» («s».getD default)",
    ] {
        assert!(laws.contains(call), "{call}:\n{laws}");
    }
    assert!(compiled(root, "app/Shared/Shape.sharp").exists());
}

/// Spec section 28: a law covers every status and event through an enum's pure transition method, whose `match`
/// gives the next status.
#[test]
fn compile_proves_a_transition_law_over_an_enum_match() {
    let directory = lawful_project(&[
        (
            "app/Shop/Payment.sharp",
            "namespace App.Shop;\n\npublic enum Payment\n{\n    case Captured;\n    case Returned;\n}\n",
        ),
        (
            "app/Shop/Status.sharp",
            "namespace App.Shop;\n\npublic enum Status : string\n{\n    case Open = \"open\";\n    case Paid = \"paid\";\n    case Refunded = \"refunded\";\n\n    public Status after(Payment e) => match (this) {\n        Status.Open => e == Payment.Captured ? Status.Paid : Status.Open,\n        Status.Paid => e == Payment.Returned ? Status.Refunded : Status.Paid,\n        Status.Refunded => Status.Refunded,\n    };\n\n    law refundedIsFinal(Payment e) => Status.Refunded.after(e) == Status.Refunded;\n}\n",
        ),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert_eq!(source_lean_files(root), Vec::<PathBuf>::new());
    assert!(compiled(root, "app/Shop/Status.sharp").exists());
}

/// A law reaches an enum's `cases()`, the list of its cases in the order the enum declares them, so a claim of another
/// order is not proven.
#[test]
fn a_law_reaches_an_enum_s_cases_in_declaration_order() {
    let level = |claim: &str| {
        format!(
            "namespace App.Auth;\n\npublic enum Level\n{{\n    case Low;\n    case Mid;\n    case High;\n\n    public static Level lowest() => match (Level.cases()) {{\n        [Level first, ...List<Level> rest] => first,\n        default => Level.High,\n    }};\n\n    law lowestIsDeclaredFirst(int unused) => {claim};\n}}\n"
        )
    };
    let directory = library_project(&[(
        "app/Auth/Level.sharp",
        &level("Level.lowest() == Level.Low && Level.cases().count() == 3"),
    )]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Auth/Level.sharp").exists());

    let directory = library_project(&[("app/Auth/Level.sharp", &level("Level.lowest() == Level.High"))]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "error[law-unproven]: law lowestIsDeclaredFirst is not proven\n = still to show: Level.lowest() == Level.High\n"
        ),
        "{}",
        printed(&output)
    );
}

/// A backed enum `Status` stating `laws`.
fn backed_status(laws: &str) -> String {
    format!(
        "namespace App.Shop;\n\npublic enum Status : string\n{{\n    case Open = \"open\";\n    case Paid = \"paid\";\n\n    {laws}\n}}\n"
    )
}

/// A law reaches a backed enum's `from`, the case whose value matches, which throws `ValueError` for any other value as
/// the engine does, and its `tryFrom`, the case or `null`. A false law over either is not proven.
#[test]
fn a_law_reaches_a_backed_enum_s_from_and_try_from() {
    let directory = lawful_project(&[(
        "app/Shop/Status.sharp",
        &backed_status(
            "law fromValueGivesTheCase(Status s) => Status.from(s.value) == s;\n    law tryFromOfAnUnknownValueIsNull(int unused) => Status.tryFrom(\"lost\") == null;",
        ),
    )]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shop/Status.sharp").exists());

    for (law, claim) in [
        ("fromOpenIsPaid", "Status.from(\"open\") == Status.Paid"),
        ("tryFromOfPaidIsNull", "Status.tryFrom(\"paid\") == null"),
    ] {
        let directory =
            lawful_project(&[("app/Shop/Status.sharp", &backed_status(&format!("law {law}(int unused) => {claim};")))]);

        let output = compile(directory.path());

        assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
        assert!(
            printed(&output)
                .contains(&format!("error[law-unproven]: law {law} is not proven\n = still to show: {claim}\n")),
            "{}",
            printed(&output)
        );
    }
}

/// An enum whose pure transition method gives the next status for every status and event, and a law over it.
const STATUS_TRANSITION: &str = "namespace App.Shop;\n\npublic enum Status : string\n{\n    case Open = \"open\";\n    case Paid = \"paid\";\n    case Refunded = \"refunded\";\n\n    public Status after(Payment e) => match (this) {                               // pure: the next status for every status and event\n        Status.Open => Status.Paid,\n        Status.Paid => Status.Refunded,\n        Status.Refunded => Status.Refunded,\n    };\n    law refundedIsFinal(Payment e) => Status.Refunded.after(e) == Status.Refunded;\n}\n";

/// Spec section 28: a transition law whose `Payment` is an interface holds for every class that implements it.
#[test]
fn compile_proves_a_transition_law_over_an_interface() {
    let directory = lawful_project(&[
        ("app/Shop/Payment.sharp", "namespace App.Shop;\n\npublic interface Payment\n{\n}\n"),
        ("app/Shop/Status.sharp", STATUS_TRANSITION),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert_eq!(source_lean_files(root), Vec::<PathBuf>::new());
    assert!(compiled(root, "app/Shop/Status.sharp").exists());
}

/// Spec section 28: a transition law called with a class that implements `Payment` as its event: Lean takes the
/// class's value as a `Payment` it knows nothing about, so the law proves for every event.
#[test]
fn compile_proves_a_transition_law_with_a_class_as_its_event() {
    let directory = lawful_project(&[
        ("app/Shop/Payment.sharp", "namespace App.Shop;\n\npublic interface Payment\n{\n}\n"),
        ("app/Shop/Status.sharp", STATUS_TRANSITION),
        (
            "app/Shop/Card.sharp",
            "namespace App.Shop;\n\npublic class Card : Payment\n{\n    public Card(public int cents { get; }) { }\n\n    law cardRefundIsFinal(int cents) => Status.Refunded.after(new Card(cents)) == Status.Refunded;\n}\n",
        ),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shop/Card.sharp").exists());
    assert_eq!(source_lean_files(root), Vec::<PathBuf>::new());
}

/// The machine of spec section 20.2's specimen, whose `pay` runs `check` before it gives `Paid`.
fn order_status(check: &str) -> String {
    format!(
        "namespace App.Shop;\n\nextern ValueError uses none;\n\npublic machine OrderStatus : string\n{{\n    case Draft = \"draft\"\n    {{\n        public OrderStatus.Paid pay(int cents)\n        {{\n{check}            return OrderStatus.Paid(cents);\n        }}\n\n        public OrderStatus.Cancelled cancel() => OrderStatus.Cancelled;\n    }}\n\n    case Paid(int cents) = \"paid\"\n    {{\n        public OrderStatus.Shipped ship() => OrderStatus.Shipped;\n    }}\n\n    final case Shipped = \"shipped\";\n    final case Cancelled = \"cancelled\";\n\n    public static OrderStatus.Draft open() => OrderStatus.Draft;\n\n    law paidIsPositive(OrderStatus.Paid paid) => paid.cents > 0;\n}}\n"
    )
}

/// Spec section 20.2: an invariant of a machine, a law whose one parameter is a case, is proved after each entry point
/// and each move that gives the case. The specimen's `pay` throws on a payment that isn't positive, so
/// `paidIsPositive` holds after it.
#[test]
fn compile_proves_a_machine_s_invariant_after_each_move_that_gives_its_case() {
    let checked = "            if (cents <= 0) {\n                throw new ValueError(\"A payment is positive.\");\n            }\n";
    let directory = lawful_project(&[("app/Shop/OrderStatus.sharp", &order_status(checked))]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shop/OrderStatus.sharp").exists());
}

/// Spec section 20.2: a move that breaks an invariant is refused with the moves that break it, the first the checker
/// finds breadth-first from each entry point, on small values of each parameter.
#[test]
fn a_move_that_breaks_a_machine_s_invariant_is_refused_with_the_moves_that_break_it() {
    let directory = lawful_project(&[("app/Shop/OrderStatus.sharp", &order_status(""))]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "error[law-unproven]: `paidIsPositive` breaks after `open() → Draft`, `pay(cents: 0) → Paid(cents: 0)`\n"
        ),
        "{}",
        printed(&output)
    );
}

/// Spec sections 20.2 and 28: an invariant is about the values entry points and moves reach, so each move keeps it from
/// a value it holds on, and a value no entry point and no move reaches never breaks it. An invariant a reachable move
/// breaks is still refused with the moves that break it.
#[test]
fn a_machine_s_invariant_holds_on_the_case_each_move_starts_from() {
    let counter = |law: &str| {
        format!(
            "namespace App;\n\npublic machine Counter\n{{\n    case Idle {{ public Counter.Counting start() => Counter.Counting(1); }}\n    case Counting(int n) {{ public Counter step() => this.n >= 2 ? Counter.Done : Counter.Counting(this.n + 1); }}\n    final case Done;\n    public static Counter.Idle idle() => Counter.Idle;\n    {law}\n}}\n"
        )
    };
    let directory = lawful_project(&[(
        "app/Counter.sharp",
        &counter("law aCountIsOneOrTwo(Counter.Counting counting) => counting.n >= 1 && counting.n <= 2;"),
    )]);

    let output = compile(directory.path());

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(directory.path(), "app/Counter.sharp").exists());

    let directory = lawful_project(&[(
        "app/Counter.sharp",
        &counter("law aCountIsOne(Counter.Counting counting) => counting.n == 1;"),
    )]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "error[law-unproven]: `aCountIsOne` breaks after `idle() → Idle`, `start() → Counting(n: 1)`, `step() → Counting(n: 2)`\n"
        ),
        "{}",
        printed(&output)
    );
}

/// Spec section 20.2: a move that breaks an invariant only on a value no small value of its parameters is, as
/// `pay(7)`, is refused by its name and the names of its parameters.
#[test]
fn a_move_that_breaks_a_machine_s_invariant_on_no_small_value_is_refused_by_name() {
    let checked = "            if (cents <= 0) {\n                throw new ValueError(\"A payment is positive.\");\n            }\n            if (cents == 7) {\n                return OrderStatus.Paid(0);\n            }\n";
    let directory = lawful_project(&[("app/Shop/OrderStatus.sharp", &order_status(checked))]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains("error[law-unproven]: `paidIsPositive` breaks after `pay(cents)`\n"),
        "{}",
        printed(&output)
    );
}

/// `App.Runs.Run`, a machine whose `Open` case holds ids and the instant it goes idle at, which its entry point `open`
/// starts with one id, beside the static `extra`.
fn run(extra: &str) -> String {
    format!(
        "namespace App.Runs;\n\nimport Sharp.Key;\nimport Sharp.Tag;\nimport Sharp.Time.Instant;\n\n[Tag(\"type\")]\npublic machine Run\n{{\n    [Key(\"open\")] case Open(List<int> ids, Instant idleAt)\n    {{\n        public Run.Idle idle() => Run.Idle(this.ids);\n    }}\n\n    [Key(\"idle\")] final case Idle(List<int> ids);\n\n    public static Run.Open open(int id, Instant idleAt) => Run.Open([id], idleAt);\n\n    {extra}\n\n    law anOpenRunHoldsAnId(Run.Open open) => open.ids.count() > 0;\n}}\n"
    )
}

/// Spec sections 20.2 and 28: only entry points and moves build a case, so every value of a machine a program holds
/// keeps its invariants, and an entry point that takes one, as `at`, keeps them from the value it takes. An entry
/// point that builds a case breaking one is refused with the goal still to show, and so is a static that takes no
/// value of the machine.
#[test]
fn an_entry_point_keeps_a_machine_s_invariant_from_the_value_it_takes() {
    let directory = library_project(&[(
        "app/Runs/Run.sharp",
        &run(
            "public static Run at(Run status, Instant now) => status is Run.Open open ? (now >= open.idleAt ? Run.Idle(open.ids) : open) : status;",
        ),
    )]);

    let output = compile(directory.path());

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(directory.path(), "app/Runs/Run.sharp").exists());

    let directory = library_project(&[(
        "app/Runs/Run.sharp",
        &run(
            "public static Run at(Run status, Instant now) => status is Run.Open open ? (now >= open.idleAt ? Run.Idle(open.ids) : Run.Open([], now)) : status;",
        ),
    )]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "error[law-unproven]: `anOpenRunHoldsAnId` breaks after `at(status, now)`\n = when status is Run.Open, still to show: open.ids.count() > 0\n"
        ),
        "{}",
        printed(&output)
    );

    let directory =
        library_project(&[("app/Runs/Run.sharp", &run("public static Run.Open empty(Instant t) => Run.Open([], t);"))]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "error[law-unproven]: `anOpenRunHoldsAnId` breaks after `empty(t)`\n = still to show: open.ids.count() > 0\n"
        ),
        "{}",
        printed(&output)
    );
}

/// `App.Count`, a machine whose `Counting` case holds a count its invariant keeps positive, which `start` builds as
/// `Counting(1)` and `at` takes and gives back, with the moves `moves` in `Counting`'s block beside `stop`.
fn count(moves: &str) -> String {
    format!(
        "namespace App;\n\npublic machine Count\n{{\n    case Counting(int n)\n    {{\n        public Count.Done stop() => Count.Done;\n{moves}    }}\n\n    final case Done;\n\n    public static Count.Counting start() => Count.Counting(1);\n\n    public static Count at(Count c) => c;\n\n    law positive(Count.Counting counting) => counting.n > 0;\n}}\n"
    )
}

/// Spec section 20.2: a program holds only values of a machine that keep its invariants, so the search for the moves
/// that break one starts an entry point or a move that takes a value of the machine only from such a value, as `at`
/// here never from `Counting(n: 0)`. A move that breaks the invariant from a value that keeps it is still refused with
/// the moves that break it.
#[test]
fn a_trace_takes_only_values_of_a_machine_that_keep_its_invariants() {
    let directory = lawful_project(&[("app/Count.sharp", &count(""))]);

    let output = compile(directory.path());

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(directory.path(), "app/Count.sharp").exists());

    let directory = lawful_project(&[(
        "app/Count.sharp",
        &count("        public Count.Counting down() => Count.Counting(this.n - 1);\n"),
    )]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "error[law-unproven]: `positive` breaks after `start() → Counting(n: 1)`, `down() → Counting(n: 0)`\n"
        ),
        "{}",
        printed(&output)
    );
}

/// `App.Trail`, a machine whose `Open` case holds ids its invariant keeps non-empty, and whose move `finish` gives the
/// `Done` that `finish` builds.
fn trail(finish: &str) -> String {
    format!(
        "namespace App;\n\npublic machine Trail\n{{\n    case Open(List<int> ids)\n    {{\n        public Trail.Done finish() => {finish};\n    }}\n\n    final case Done(List<int> ids);\n\n    public static Trail.Open start(int id) => Trail.Open([id]);\n\n    law openHoldsOne(Trail.Open o) => o.ids.count() > 0;\n\n    law doneHoldsOne(Trail.Done d) => d.ids.count() > 0;\n}}\n"
    )
}

/// Spec section 20.2: a move or an entry point that gives a case keeps its invariant from every invariant of the case
/// it starts from, whichever case it gives and whichever invariant of its own case it needs. A move that breaks the
/// invariant of the case it gives is still refused with the moves that break it.
#[test]
fn a_machine_s_invariant_holds_from_every_invariant_of_the_case_a_move_starts_from() {
    let directory = library_project(&[
        ("app/Trail.sharp", &trail("Trail.Done(this.ids)")),
        (
            "app/Tally.sharp",
            "namespace App;\n\npublic machine Tally\n{\n    case Open(int n)\n    {\n        public Tally.Done finish() => Tally.Done(this.n);\n    }\n\n    final case Done(int n);\n\n    public static Tally.Open start() => Tally.Open(1);\n\n    law openIsPositive(Tally.Open o) => o.n > 0;\n\n    law doneIsPositive(Tally.Done d) => d.n > 0;\n}\n",
        ),
        (
            "app/Ledger.sharp",
            "namespace App;\n\npublic machine Ledger\n{\n    final case Open(List<int> ids);\n\n    final case Closed(List<int> ids);\n\n    public static Ledger.Open start(int id) => Ledger.Open([id]);\n\n    public static Ledger close(Ledger ledger) => ledger is Ledger.Open open ? Ledger.Closed(open.ids) : ledger;\n\n    law openHoldsOne(Ledger.Open open) => open.ids.count() > 0;\n\n    law closedHoldsOne(Ledger.Closed closed) => closed.ids.count() > 0;\n}\n",
        ),
        (
            "app/Climb.sharp",
            "namespace App;\n\npublic machine Climb\n{\n    case Counting(int n)\n    {\n        public Climb.Counting up() => Climb.Counting(this.n + 1);\n\n        public Climb.Done stop() => Climb.Done;\n    }\n\n    final case Done;\n\n    public static Climb.Counting start() => Climb.Counting(1);\n\n    law positive(Climb.Counting counting) => counting.n > 0;\n\n    law nonZero(Climb.Counting counting) => counting.n != 0;\n}\n",
        ),
    ]);

    let output = compile(directory.path());

    assert!(output.status.success(), "{}", printed(&output));
    for file in ["app/Trail.sharp", "app/Tally.sharp", "app/Ledger.sharp", "app/Climb.sharp"] {
        assert!(compiled(directory.path(), file).exists(), "{file}");
    }

    let directory = library_project(&[("app/Trail.sharp", &trail("Trail.Done([])"))]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "error[law-unproven]: `doneHoldsOne` breaks after `start(id: 0) → Open(ids: …)`, `finish() → Done(ids: …)`\n"
        ),
        "{}",
        printed(&output)
    );
}

/// A law reads a nullable `int` with `??` and `==` as the integer it holds, whichever branch built it, so a claim over
/// a case field `int?` proves as one over an `int` does.
#[test]
fn a_law_reads_a_nullable_int_with_coalesce_and_equality_as_the_integer_it_holds() {
    let directory = lawful_project(&[
        (
            "app/Spots.sharp",
            "namespace App;\n\npublic machine Spots\n{\n    final case Sold;\n    final case Open(int? left);\n\n    public static Spots of(int? cap, int claims)\n        => cap != null && claims >= cap ? Spots.Sold : (cap == null ? Spots.Open(null) : Spots.Open(cap - claims));\n\n    law leftIsNeverNegative(Spots.Open open) => (open.left ?? 0) >= 0;\n\n    law openCountsItsSpots(int cap, int claims) when claims < cap && claims >= 0 => Spots.of(cap, claims) is Spots.Open open ? open.left == cap - claims : false;\n}\n",
        ),
        (
            "app/Seats.sharp",
            "namespace App;\n\npublic machine Seats\n{\n    final case Full;\n    final case Open(int? left);\n\n    public static Seats of(int n) => n > 0 ? Seats.Open(n - 1) : (n == 0 ? Seats.Open(null) : Seats.Full);\n\n    law leftIsNeverNegative(Seats.Open open) => (open.left ?? 0) >= 0;\n\n    law ofLeavesOneLess(int n) => n <= 0 || (Seats.of(n) is Seats.Open open ? open.left == n - 1 : false);\n}\n",
        ),
    ]);

    let output = compile(directory.path());

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(directory.path(), "app/Spots.sharp").exists());
    assert!(compiled(directory.path(), "app/Seats.sharp").exists());
}

/// A law reads a field of a machine with one case as it reads a field of a machine with more, so the laws that prove
/// for a machine with two cases prove for the same machine with one.
#[test]
fn a_law_reads_the_fields_of_a_machine_with_one_case() {
    let directory = lawful_project(&[
        (
            "app/Seats.sharp",
            "namespace App;\n\npublic machine Seats\n{\n    final case Open(int? left);\n\n    public static Seats.Open of(int n) => n > 0 ? Seats.Open(n - 1) : Seats.Open(null);\n\n    law leftIsNeverNegative(Seats.Open open) => (open.left ?? 0) >= 0;\n\n    law ofLeavesOneLess(int n) => n <= 0 || Seats.of(n).left == n - 1;\n}\n",
        ),
        (
            "app/Spaces.sharp",
            "namespace App;\n\npublic machine Spaces\n{\n    final case Open(int? left);\n\n    public static Spaces of(int n) => n >= 0 ? Spaces.Open(n) : Spaces.Open(null);\n\n    law leftIsNeverNegative(Spaces.Open open) => open.left is int left ? left >= 0 : true;\n\n    law ofLeavesNoneNegative(int n) => Spaces.of(n) is Spaces.Open open ? (open.left is int left ? left >= 0 : true) : true;\n}\n",
        ),
    ]);

    let output = compile(directory.path());

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(directory.path(), "app/Seats.sharp").exists());
    assert!(compiled(directory.path(), "app/Spaces.sharp").exists());
}

/// A machine whose second law names a parameter `until` and whose `Going` case holds a field `while`, two of Lean's
/// own words.
const SUBSCRIPTION_STATUS: &str = "namespace App.Repro;\n\nimport Sharp.Time.Instant;\n\npublic machine SubscriptionStatus\n{\n    case Waiting\n    {\n        public SubscriptionStatus.Going go(Instant at) => SubscriptionStatus.Going(at, 0);\n    }\n\n    case Going(Instant at, int @while)\n    {\n        public SubscriptionStatus.Paused pause() => SubscriptionStatus.Paused;\n        public SubscriptionStatus.Done finish() => SubscriptionStatus.Done;\n    }\n\n    case Paused\n    {\n        public SubscriptionStatus.Done end() => SubscriptionStatus.Done;\n    }\n\n    final case Done;\n\n    public static SubscriptionStatus.Waiting enter() => SubscriptionStatus.Waiting;\n\n    law goingHoldsTheTime(Instant at) => SubscriptionStatus.enter().go(at).at == at;\n    law pausingHoldsTheResume(Instant at, Instant until) => SubscriptionStatus.enter().go(at).pause() is SubscriptionStatus.Paused;\n}\n";

/// A PHP# name is a name to Lean whatever it is, Lean's own words `until` and `while` too, so the laws that reach it
/// prove.
#[test]
fn a_law_proves_whatever_its_parameters_and_the_fields_it_reaches_are_named() {
    let directory = library_project(&[("app/Repro/SubscriptionStatus.sharp", SUBSCRIPTION_STATUS)]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Repro/SubscriptionStatus.sharp").exists());
}

/// `Holder`, which holds a `JsonValue?`, whose `Any?` a law cannot read, beside the law `law`.
fn holder(law: &str) -> String {
    format!(
        "namespace App.Shared;\n\nimport Sharp.Encoding.JsonValue;\n\npublic final class Holder\n{{\n    public Holder(public int id {{ get; }}, public JsonValue? payload {{ get; }} = null) {{}}\n\n    {law}\n}}\n"
    )
}

/// Spec section 28: a value whose type a law cannot read is one the law knows nothing inside, so a law that only
/// builds and carries it proves, as `Holder`'s `JsonValue?` here.
#[test]
fn a_law_that_only_carries_a_value_it_cannot_read_proves() {
    let directory = library_project(&[(
        "app/Shared/Holder.sharp",
        &holder("law holdingKeepsTheId(int id) => new Holder(id, null).id == id;"),
    )]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shared/Holder.sharp").exists());
}

/// A law that reads into a value whose type a law cannot read is refused for what it reads, as `JsonValue`'s `Any?`.
#[test]
fn a_law_that_reads_into_a_value_it_cannot_read_is_refused() {
    let law = "law readsThePayload(int id, JsonValue payload) => new Holder(id, payload).payload?.value == null;";
    let directory = library_project(&[("app/Shared/Holder.sharp", &holder(law))]);
    let root = directory.path();

    let output = compile_without_lean(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "error - unmodeled-construct: Law readsThePayload reaches Any in JsonValue. It reads types while the code runs."
        ),
        "{}",
        printed(&output)
    );
    assert!(!compiled(root, "app/Shared/Holder.sharp").exists());
}

/// A law never calls an interface's method, which a law cannot reason about, even when every implementation is
/// pure as spec section 29 holds `Card.amount` to. A call through `?.` is refused too.
#[test]
fn a_law_that_calls_an_interface_method_is_refused() {
    let payment = "namespace App.Shop;\n\npublic interface Payment\n{\n    int amount();\n}\n";
    for (file, source, law, calls) in [
        (
            "app/Shop/Card.sharp",
            "namespace App.Shop;\n\npublic class Card : Payment\n{\n    public Card(public int cents { get; }) { }\n\n    public int amount() => this.cents;\n\n    public static bool same(Payment p) => p.amount() == p.amount();\n\n    law alwaysSame(Payment p) => Card.same(p);\n}\n",
            "alwaysSame reaches the interface method Payment.amount in Card.same",
            ["9:43", "9:57"],
        ),
        (
            "app/Shop/Refund.sharp",
            "namespace App.Shop;\n\npublic class Refund\n{\n    public Refund(public Payment payment { get; }, public Payment? fee { get; }) { }\n\n    public int owed() => this.payment.amount() - (this.fee?.amount() ?? 0);\n\n    law owedIsNumber(Refund r) => r.owed() == r.owed();\n}\n",
            "owedIsNumber reaches the interface method Payment.amount in Refund.owed",
            ["7:26", "7:51"],
        ),
    ] {
        let directory = lawful_project(&[("app/Shop/Payment.sharp", payment), (file, source)]);
        let root = directory.path();
        let expected: Vec<String> = calls
            .iter()
            .map(|at| format!("{file}:{at}:error - unmodeled-construct: Law {law}. Call the method on a value of a class that implements the interface, since a law cannot know which implementation runs."))
            .collect();

        let output = compile_without_lean(root);

        assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
        assert_eq!(issue_lines(&output), expected, "{}", printed(&output));
    }
}

/// `app/Integrations/IntegrationFailure.sharp`, an interface whose `value` has no storage of its own, which the backed
/// enum `Outage` implements, and `app/Integrations/Readiness.sharp`, a machine whose cases hold a failure, stating
/// `laws`.
fn readiness(laws: &str) -> tempfile::TempDir {
    lawful_project(&[
        (
            "app/Integrations/IntegrationFailure.sharp",
            "namespace App.Integrations;\n\npublic interface IntegrationFailure\n{\n    string value { get; }\n}\n",
        ),
        (
            "app/Integrations/Outage.sharp",
            "namespace App.Integrations;\n\npublic enum Outage : string, IntegrationFailure\n{\n    case Timeout = \"timeout\";\n    case Refused = \"refused\";\n}\n",
        ),
        (
            "app/Integrations/Readiness.sharp",
            &format!(
                "namespace App.Integrations;\n\npublic machine Readiness\n{{\n    final case Inactive(IntegrationFailure failure);\n    final case Waiting(IntegrationFailure? failure);\n\n    public static Readiness.Inactive inactive(IntegrationFailure failure) => Readiness.Inactive(failure);\n    public static Readiness.Waiting waiting(IntegrationFailure? failure) => Readiness.Waiting(failure);\n\n    {laws}\n}}\n"
            ),
        ),
    ])
}

/// A law reads an interface's property through the interface's reader, which every implementation answers, so a law
/// that reads the same value twice proves: through `.`, through `?.`, and in a machine's invariant.
#[test]
fn a_law_reads_an_interface_s_property_through_its_reader() {
    let directory = readiness(
        "law anInactiveIntegrationHoldsItsFailure(IntegrationFailure failure) => Readiness.inactive(failure).failure.value == failure.value;\n    law aWaitingIntegrationHoldsItsFailure(IntegrationFailure? failure) => Readiness.waiting(failure).failure?.value == failure?.value;\n    law anInactiveIntegrationReadsOneValue(Readiness.Inactive inactive) => inactive.failure.value == inactive.failure.value;",
    );
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Integrations/Readiness.sharp").exists());
}

/// A law knows of an interface's property only what holds for every implementation, so a claim that it is one fixed
/// value is not proven, and the goal still to show names the read.
#[test]
fn a_law_claiming_one_value_for_every_implementation_of_an_interface_s_property_is_not_proven() {
    let directory = readiness("law everyFailureTimesOut(IntegrationFailure failure) => failure.value == \"timeout\";");
    let root = directory.path();

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "error[law-unproven]: law everyFailureTimesOut is not proven\n = still to show: failure.value == "
        ),
        "{}",
        printed(&output)
    );
    assert!(!compiled(root, "app/Integrations/Readiness.sharp").exists());
}

/// An abstract property has no storage in its class's value, so a law reads it through the class's reader, and a
/// subclass that holds it reads its own value.
#[test]
fn a_law_reads_an_abstract_property_through_its_reader_and_a_subclass_s_own_value() {
    let directory = lawful_project(&[
        (
            "app/Integrations/Integration.sharp",
            "namespace App.Integrations;\n\npublic abstract class Integration\n{\n    public abstract string label { get; }\n\n    law anIntegrationReadsOneLabel(Integration integration) => integration.label == integration.label;\n}\n",
        ),
        (
            "app/Integrations/Stripe.sharp",
            "namespace App.Integrations;\n\npublic final class Stripe : Integration\n{\n    public override string label { get; init; } = \"stripe\";\n\n    law stripeIsLabeledStripe(int unused) => new Stripe().label == \"stripe\";\n}\n",
        ),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Integrations/Integration.sharp").exists());
    assert!(compiled(root, "app/Integrations/Stripe.sharp").exists());
}

/// `app/Shop/OrderStatus.sharp`, a sealed family of the states `Pending` and `Paid`, whose `Pending` states `laws`.
fn order_states(laws: &str) -> tempfile::TempDir {
    lawful_project(&[
        ("app/Shop/OrderStatus.sharp", "namespace App.Shop;\n\npublic sealed interface OrderStatus\n{\n}\n"),
        (
            "app/Shop/Paid.sharp",
            "namespace App.Shop;\n\npublic class Paid : OrderStatus\n{\n    public Paid(public int paidAt { get; }) { }\n}\n",
        ),
        (
            "app/Shop/Pending.sharp",
            &format!(
                "namespace App.Shop;\n\npublic class Pending : OrderStatus\n{{\n    public static Pending initial() => new Pending();\n    public static OrderStatus start() => new Pending();\n    public Paid pay(int at) => new Paid(at);\n    public OrderStatus settle(int at) => new Paid(at);\n\n    {laws}\n}}\n"
            ),
        ),
    ])
}

/// Spec section 28: a law reads a sealed family as the closed set of its implementers, so a type test over it, as an
/// `is` or a `match`, is a test of which implementer a value is, and an implementer's get-only property reads in the
/// branch the test names it in. A false claim of which state a move enters is not proven, and a test of a class against
/// another class is the checker's own error before any law runs.
#[test]
fn compile_proves_a_law_over_a_sealed_family_s_type_tests_and_its_states_data() {
    let directory = order_states(
        "law startsPending(int unused) => Pending.initial() is Pending && Pending.start() is Pending;\n    law payKeepsTime(Pending p, int at) => p.pay(at).paidAt == at;\n    law settleKeepsTime(Pending p, int at) => p.settle(at) is Paid paid ? paid.paidAt == at : false;\n    law settleIsNoLongerPending(Pending p, int at) => match (p.settle(at)) {\n        Pending pending => false,\n        Paid paid => paid.paidAt == at,\n    };",
    );
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shop/Pending.sharp").exists());
    assert_eq!(source_lean_files(root), Vec::<PathBuf>::new());

    let directory = order_states("law startIsPaid(int unused) => Pending.start() is Paid;");

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "error[law-unproven]: law startIsPaid is not proven\n = still to show: Pending.start() is Paid\n"
        ),
        "{}",
        printed(&output)
    );

    let directory = order_states("law startsPaid(int unused) => Pending.initial() is Paid;");

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "app/Shop/Pending.sharp:10:56: error[impossible-type-comparison]: This pattern never matches the value it tests.\n"
        ),
        "{}",
        printed(&output)
    );
}

/// Spec section 28: a move whose `?:`, `match` or `??` gives one state in a branch and another in the next gives a value
/// of the sealed interface, an enum's case among them, and a law tells its states apart. A state's property reads where
/// a test narrowed the value to it, and in its own methods.
#[test]
fn compile_proves_a_law_over_moves_whose_branches_give_different_states() {
    let directory = lawful_project(&[
        ("app/Shop/Status.sharp", "namespace App.Shop;\n\npublic sealed interface Status\n{\n}\n"),
        (
            "app/Shop/Open.sharp",
            "namespace App.Shop;\n\npublic enum Open : Status\n{\n    case Fresh;\n    case Reopened(int times);\n}\n",
        ),
        (
            "app/Shop/Closed.sharp",
            "namespace App.Shop;\n\npublic class Closed : Status\n{\n    public Closed(public int at { get; }) { }\n\n    public static Status close(Status status, int at) => status is Open ? new Closed(at) : status;\n    public static Status reopen(Status status) => match (status) {\n        Closed closed => Open.Reopened(closed.at),\n        Open open => open,\n    };\n    public static Status settled(Closed? closed) => closed ?? Open.Fresh;\n    public static int closedAt(Status status) => status is Closed ? status.at : 0;\n    public Closed same() => new Closed(this.at);\n\n    law closeCloses(Status status, int at) => Closed.close(status, at) is Closed;\n    law reopenCounts(int at) => Closed.reopen(new Closed(at)) is Open.Reopened(int times) ? times == at : false;\n    law freshStaysFresh(int at) => Closed.reopen(Open.Fresh) is Open;\n    law unsettledIsFresh(int unused) => Closed.settled(null) is Open;\n    law closeKeepsTime(int at) => Closed.closedAt(Closed.close(Open.Fresh, at)) == at;\n    law sameKeepsTime(Closed closed) => closed.same().at == closed.at;\n}\n",
        ),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shop/Closed.sharp").exists());
}

/// Spec section 28: a law over a sealed interface that no class implements proves, as one over any other interface's
/// value does, though the interface has no implementer to tell apart.
#[test]
fn compile_proves_a_law_over_a_sealed_interface_no_class_implements() {
    let directory = lawful_project(&[
        ("app/Shop/Nothing.sharp", "namespace App.Shop;\n\npublic sealed interface Nothing\n{\n}\n"),
        (
            "app/Shop/Uses.sharp",
            "namespace App.Shop;\n\npublic static class Uses\n{\n    public static bool keep(Nothing value) => true;\n\n    law keeps(Nothing value) => Uses.keep(value);\n}\n",
        ),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shop/Uses.sharp").exists());
}

/// Spec section 28: a law reaches a standard library type from its PHP# source, as it reaches a project type, so a law
/// over one state of a sealed family proves though another state holds an `Instant`.
#[test]
fn compile_proves_a_law_over_a_state_beside_one_that_holds_an_instant() {
    let directory = library_project(&[
        (
            "app/Delivery/DeliveryStatus.sharp",
            "namespace App.Delivery;\n\npublic sealed interface DeliveryStatus\n{\n}\n",
        ),
        (
            "app/Delivery/Pending.sharp",
            "namespace App.Delivery;\n\npublic final class Pending : DeliveryStatus\n{\n    public Pending(public string? error { get; }) { }\n\n    public Pending postpone(string error) => new Pending(error);\n\n    law postponeHoldsTheError(string? held, string error) => new Pending(held).postpone(error).error == error;\n}\n",
        ),
        (
            "app/Delivery/Done.sharp",
            "namespace App.Delivery;\n\nimport Sharp.Time.Instant;\n\npublic final class Done : DeliveryStatus\n{\n    internal Done(public Instant doneAt { get; }) { }\n}\n",
        ),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Delivery/Pending.sharp").exists());
}

/// A theorem rewrites with the equations Lean built for a match of its module's own definitions, as the module holds
/// them, and builds none again: Lean refuses a second constant of one name with a panic, whose backtrace costs the law
/// checker more than the proof. `LEAN_ABORT_ON_PANIC` ends the runner at a panic, so the compile fails where one is
/// raised.
#[test]
fn a_machine_s_laws_prove_with_no_panic_in_the_law_checker() {
    let directory = library_project(&[(
        "app/Seats/SeatStatus.sharp",
        include_str!("../../../benchmark/laws/Seats/SeatStatus.sharp"),
    )]);
    let root = directory.path();

    let output = process::output(command(root, "never").env("LEAN_ABORT_ON_PANIC", "1")).unwrap();

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Seats/SeatStatus.sharp").exists());
}

/// Spec section 28: a state of a sealed family passes where an open interface it implements is expected, so a law
/// proves that passes one there, and two states that implement the same open interface both pass as it.
#[test]
fn compile_proves_a_law_that_passes_a_sealed_state_as_an_open_interface_it_implements() {
    let directory = lawful_project(&[
        ("app/Shop/Printable.sharp", "namespace App.Shop;\n\npublic interface Printable\n{\n}\n"),
        ("app/Shop/OrderStatus.sharp", "namespace App.Shop;\n\npublic sealed interface OrderStatus\n{\n}\n"),
        (
            "app/Shop/Paid.sharp",
            "namespace App.Shop;\n\npublic class Paid : OrderStatus, Printable\n{\n    public Paid(public int at { get; }) { }\n}\n",
        ),
        (
            "app/Shop/Pending.sharp",
            "namespace App.Shop;\n\npublic class Pending : OrderStatus, Printable\n{\n    public static bool keep(Printable value) => true;\n\n    law keepsPending(Pending pending) => Pending.keep(pending);\n    law keepsPaid(int at) => Pending.keep(new Paid(at));\n}\n",
        ),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shop/Pending.sharp").exists());
}

/// Spec section 28: a law refuses a class that is a state of two sealed interfaces, as whichever one it passes the
/// value as, since a state belongs to one sealed family in a law.
#[test]
fn a_law_reaching_a_state_of_two_sealed_interfaces_is_refused() {
    let source = "namespace App.Shared;\n\npublic class Shape : Drawn, Colored\n{\n    public Shape(public int size { get; }) { }\n\n    public static bool keepColored(Colored colored) => true;\n    public static bool keepDrawn(Drawn drawn) => true;\n\n    law keepsColored(Shape shape) => Shape.keepColored(shape);\n    law keepsDrawn(Shape shape) => Shape.keepDrawn(shape);\n}\n";
    let others = [
        ("app/Shared/Colored.sharp", "namespace App.Shared;\n\npublic sealed interface Colored\n{\n}\n"),
        ("app/Shared/Drawn.sharp", "namespace App.Shared;\n\npublic sealed interface Drawn\n{\n}\n"),
    ];

    for law in ["keepsColored", "keepsDrawn"] {
        assert_unmodeled_beside(
            source,
            &others,
            &format!(
                "Law {law} reaches the class Shape in Shape.{law}. It is a state of both `Colored` and `Drawn`, and a state belongs to one sealed interface in a law."
            ),
        );
    }
}

/// Spec sections 20.2 and 28: a machine's cases are every value it has, so a law reasons about `is` on a machine
/// value, `this` included, and about `==` between two of its values, which compares the case and its fields. A law
/// that is false in a case is not proven.
#[test]
fn a_law_reasons_about_a_machine_value_s_case_tests_and_equality() {
    let lamp = |laws: &str| {
        format!(
            "namespace App;\n\npublic machine Lamp\n{{\n    case Off {{ public Lamp.On turnOn() => Lamp.On; }}\n    final case On;\n    public static Lamp.Off off() => Lamp.Off;\n    public bool lit() => this is Lamp.On;\n    {laws}\n}}\n"
        )
    };
    let walk = |laws: &str| {
        format!(
            "namespace App;\n\npublic machine Walk\n{{\n    case A {{ public Walk.B go() => Walk.B; }}\n    case B {{ public Walk.A back() => Walk.A; }}\n    public static Walk.A enter() => Walk.A;\n    {laws}\n}}\n"
        )
    };
    let toggle = "namespace App;\n\npublic machine Lamp\n{\n    case Off { public Lamp.On turnOn() => Lamp.On; }\n    case On { public Lamp.Off turnOff() => Lamp.Off; }\n    public static Lamp.Off enter() => Lamp.Off;\n    law turningOnLightsTheLamp() => Lamp.enter().turnOn() == Lamp.On;\n}\n";
    for (name, source) in [
        (
            "app/Lamp.sharp",
            lamp(
                "law turningOnLights() => Lamp.off().turnOn().lit();\n    law turningOnLightsTheLamp() => Lamp.off().turnOn() == Lamp.On;",
            ),
        ),
        ("app/Lamp.sharp", toggle.to_owned()),
        ("app/Walk.sharp", walk("law roundTrip() => Walk.enter().go().back() == Walk.enter();")),
    ] {
        let directory = lawful_project(&[(name, &source)]);

        let output = compile(directory.path());

        assert!(output.status.success(), "{}", printed(&output));
        assert!(compiled(directory.path(), name).exists());
    }
    for (name, source, law) in [
        ("app/Lamp.sharp", lamp("law dark() => Lamp.off().turnOn().lit() == false;"), "dark"),
        ("app/Walk.sharp", walk("law wrong() => Walk.enter().go() == Walk.enter();"), "wrong"),
    ] {
        let directory = lawful_project(&[(name, &source)]);

        let output = compile(directory.path());

        assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
        assert!(
            printed(&output).contains(&format!("error[law-unproven]: law {law} is not proven\n")),
            "{}",
            printed(&output)
        );
    }
}

/// Spec sections 21 and 28: a type test is a condition like any pattern's, so `or`, `and` and `not` combine it, of a
/// sealed interface's implementer, of a machine's case and of a list. A law that is false where the combined test holds
/// is not proven.
#[test]
fn a_law_reasons_about_a_type_test_inside_or_and_not() {
    let light = |answer: &str, law: &str| {
        format!(
            "namespace App;\n\npublic machine Light\n{{\n    case Red(int at)\n    {{\n        public Light.Red answer(Signal signal, int now) => {answer};\n    }}\n\n    public static Light.Red enter(int at) => Light.Red(at);\n\n    {law}\n}}\n"
        )
    };
    let lamp = "namespace App;\n\npublic machine Lamp\n{\n    case Off { public Lamp.On turnOn() => Lamp.On; }\n    final case On;\n    public static Lamp off() => Lamp.Off;\n    public bool known() => this is Lamp.Off or Lamp.On;\n    law offIsKnown() => Lamp.off().known();\n}\n";
    let count = "namespace App;\n\npublic class Count\n{\n    public static int few(List<int> xs) => xs is [] or [int] ? 1 : 2;\n    law oneIsFew(int x) => Count.few([x]) == 1;\n    law twoAreMany(int x) => Count.few([x, x]) == 2;\n}\n";
    let signals = |light: String| {
        lawful_project(&[
            ("app/Signal.sharp", "namespace App;\n\npublic sealed interface Signal\n{\n}\n"),
            ("app/Go.sharp", "namespace App;\n\npublic class Go : Signal\n{\n}\n"),
            (
                "app/Stop.sharp",
                "namespace App;\n\npublic class Stop : Signal\n{\n    public Stop(public string reason { get; }) { }\n}\n",
            ),
            ("app/Wait.sharp", "namespace App;\n\npublic class Wait : Signal\n{\n}\n"),
            ("app/Light.sharp", &light),
            ("app/Lamp.sharp", lamp),
            ("app/Count.sharp", count),
        ])
    };
    let or = "match (signal) {\n            Go => Light.Red(now),\n            Stop or Wait => this,\n        }";
    let not = "signal is not Go ? this : Light.Red(now)";
    for light in [
        light(or, "law stopKeeps(int now) => Light.enter(1).answer(new Stop(\"r\"), now).at == 1;"),
        light(not, "law waitKeeps(int now) => Light.enter(1).answer(new Wait(), now).at == 1;"),
    ] {
        let directory = signals(light);

        let output = compile(directory.path());

        assert!(output.status.success(), "{}", printed(&output));
        for name in ["app/Light.sharp", "app/Lamp.sharp", "app/Count.sharp"] {
            assert!(compiled(directory.path(), name).exists(), "{name}");
        }
    }
    for (light, law) in [
        (light(or, "law waitMoves(int now) => Light.enter(1).answer(new Wait(), now).at == now;"), "waitMoves"),
        (light(not, "law stopMoves(int now) => Light.enter(1).answer(new Stop(\"r\"), now).at == now;"), "stopMoves"),
    ] {
        let directory = signals(light);

        let output = compile(directory.path());

        assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
        assert!(
            printed(&output).contains(&format!("error[law-unproven]: law {law} is not proven\n")),
            "{}",
            printed(&output)
        );
    }
}

/// Spec sections 20.2 and 28: a union of one machine's cases is a part of the machine's closed set of cases, so a law
/// reasons about a move that gives one of them, and about which case it gives. A law that is false in the case the move
/// gives is not proven.
#[test]
fn a_law_reasons_about_a_union_of_one_machine_s_cases() {
    let counter = |law: &str| {
        format!(
            "namespace App;\n\npublic machine Counter\n{{\n    case Counting(int left)\n    {{\n        public Counter.Counting|Counter.Done step() => this.left > 1 ? Counter.Counting(this.left - 1) : Counter.Done;\n    }}\n\n    final case Done;\n\n    public static Counter.Counting start(int left) => Counter.Counting(left);\n\n    {law}\n}}\n"
        )
    };

    let directory = lawful_project(&[(
        "app/Counter.sharp",
        &counter(
            "law twoCount() => Counter.start(2).step() is Counter.Counting;\n    law oneIsDone() => Counter.start(1).step() is Counter.Done;",
        ),
    )]);

    let output = compile(directory.path());

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(directory.path(), "app/Counter.sharp").exists());

    let directory = lawful_project(&[(
        "app/Counter.sharp",
        &counter("law oneCounts() => Counter.start(1).step() is Counter.Counting;"),
    )]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(printed(&output).contains("error[law-unproven]: law oneCounts is not proven\n"), "{}", printed(&output));
}

/// Spec sections 20.2 and 28: a value typed as a union of one machine's cases is a value of the machine, so a law calls
/// a move every case of the union moves from on it, as the case it holds runs the move, and tests which case it is. A
/// law that is false in the case the moves give is not proven.
#[test]
fn a_law_follows_a_value_typed_as_a_union_of_one_machine_s_cases() {
    let step = |law: &str| {
        format!(
            "namespace App;\n\ninternal machine Step\n{{\n    case A {{ internal Step.A|Step.B go(bool done) => done ? Step.B : Step.A; }}\n    case B {{ internal Step.B|Step.C go(bool done) => done ? Step.C : Step.B; }}\n    final case C;\n    internal static Step.A enter() => Step.A;\n    {law}\n}}\n"
        )
    };
    let gate = "namespace App;\n\ninternal machine Gate\n{\n    case Open { internal Gate.Shut|Gate.Gone close(bool forever) => forever ? Gate.Gone : Gate.Shut; }\n    case Shut { internal Gate.Open open() => Gate.Open; }\n    final case Gone;\n    internal static Gate.Open enter() => Gate.Open;\n    law shutReopens() => Gate.enter().close(false) is Gate.Shut s ? s.open() is Gate.Open : false;\n}\n";

    let directory = lawful_project(&[
        ("app/Step.sharp", &step("law twoMovesReachC() => Step.enter().go(true).go(true) is Step.C;")),
        ("app/Gate.sharp", gate),
    ]);

    let output = compile(directory.path());

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(directory.path(), "app/Step.sharp").exists());
    assert!(compiled(directory.path(), "app/Gate.sharp").exists());

    let directory = lawful_project(&[(
        "app/Step.sharp",
        &step("law twoWaitsReachC() => Step.enter().go(false).go(false) is Step.C;"),
    )]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains("error[law-unproven]: law twoWaitsReachC is not proven\n"),
        "{}",
        printed(&output)
    );
}

/// Spec section 28: `<` on two values of a class runs the class's own `<=>`, so a law that compares two `Instant`s
/// proves through `Instant`'s, which compares their seconds, then their nanoseconds. A false order is not proven.
#[test]
fn compile_proves_a_law_that_orders_two_instants_through_their_own_comparison() {
    let schedule = |laws: &str| {
        format!(
            "namespace App.Shared;\n\nimport Sharp.Time.Instant;\n\npublic final class Schedule\n{{\n    {laws}\n}}\n"
        )
    };
    let directory = library_project(&[(
        "app/Shared/Schedule.sharp",
        &schedule(
            "law earlierSecondIsEarlier(Instant a, Instant b) when a.epochSeconds < b.epochSeconds => a < b;\n    law sameSecondByNanosecond(Instant a, Instant b) when a.epochSeconds == b.epochSeconds && a.nanosecond < b.nanosecond => a < b;",
        ),
    )]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shared/Schedule.sharp").exists());

    let directory = library_project(&[(
        "app/Shared/Schedule.sharp",
        &schedule("law laterNanosecondIsEarlier(Instant a, Instant b) when a.nanosecond > b.nanosecond => a < b;"),
    )]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains("error[law-unproven]: law laterNanosecondIsEarlier is not proven\n"),
        "{}",
        printed(&output)
    );
}

/// `App.Shared.Later`, a class of `laws` over an `Instant` the law takes.
fn later(laws: &str) -> String {
    format!(
        "namespace App.Shared;\n\nimport Sharp.Time.Duration;\nimport Sharp.Time.Instant;\n\npublic final class Later\n{{\n    {laws}\n}}\n"
    )
}

/// Spec section 28: a constructor's checks are a precondition of the value, so every `Instant` a law takes has a
/// nanosecond in `Instant`'s range, and five minutes after it is later than it. At a nanosecond the constructor
/// refuses, the law would be false.
#[test]
fn a_law_over_an_instant_it_takes_reads_the_checks_of_instant_s_constructor() {
    let directory = library_project(&[(
        "app/Shared/Later.sharp",
        &later("law laterIsAfter(Instant at) => at.plus(Duration.ofMinutes(5)) > at;"),
    )]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shared/Later.sharp").exists());
}

/// A law over an `Instant` that is false for an `Instant` the constructor builds is refused with its claim, though
/// every `Instant` it takes keeps the constructor's checks.
#[test]
fn a_false_law_over_an_instant_its_constructor_builds_is_not_proven() {
    let directory = library_project(&[(
        "app/Shared/Later.sharp",
        &later("law laterIsTheSame(Instant at) => at.plus(Duration.ofMinutes(5)) == at;"),
    )]);
    let root = directory.path();

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains("error[law-unproven]: law laterIsTheSame is not proven\n = still to show: new Instant(at.epochSeconds + new Duration(5 * 60000000000).nanoseconds.floorDiv(1000000000) + "),
        "{}",
        printed(&output)
    );
    assert!(!compiled(root, "app/Shared/Later.sharp").exists());
}

/// A machine whose cases hold nullable `Instant`s, as an offer's availability does: `at` compares `now` with the
/// instants an offer starts and ends at through `Instant`'s `operator <=>`, an `if` on the seconds, under the `if`s of
/// its `is` tests.
const OFFER_AVAILABILITY: &str = "namespace App.Offers;\n\nimport Sharp.Time.Instant;\n\npublic machine OfferAvailability : Variant\n{\n    final case Available(Instant? startsAt, Instant? endsAt, int? spotsRemaining);\n    final case Inactive;\n    final case Undeliverable;\n    final case NotStarted;\n    final case Ended(Instant endedAt);\n    final case SoldOut(Instant? startsAt, Instant? endsAt);\n    final case Purchased(Instant? startsAt, Instant? endsAt);\n\n    public static OfferAvailability of(bool active, bool deliverable, Instant? startsAt, Instant? deadline, int? maxBuyers, int claims, int? maxPerCustomer, int? customerClaims) {\n        if (!active) { return OfferAvailability.Inactive; }\n        if (!deliverable) { return OfferAvailability.Undeliverable; }\n        if (maxBuyers != null && claims >= maxBuyers) { return OfferAvailability.SoldOut(startsAt, deadline); }\n        if (maxPerCustomer != null && customerClaims != null && customerClaims >= maxPerCustomer) { return OfferAvailability.Purchased(startsAt, deadline); }\n        if (maxBuyers == null) { return OfferAvailability.Available(startsAt, deadline, null); }\n        return OfferAvailability.Available(startsAt, deadline, maxBuyers - claims);\n    }\n\n    public static OfferAvailability at(OfferAvailability status, Instant now)\n        => status is OfferAvailability.Available available\n            ? OfferAvailability.timed(available, available.startsAt, available.endsAt, now)\n            : (status is OfferAvailability.SoldOut soldOut\n                ? OfferAvailability.timed(soldOut, soldOut.startsAt, soldOut.endsAt, now)\n                : (status is OfferAvailability.Purchased purchased ? OfferAvailability.timed(purchased, purchased.startsAt, purchased.endsAt, now) : status));\n\n    private static OfferAvailability timed(OfferAvailability open, Instant? startsAt, Instant? endsAt, Instant now)\n        => startsAt is Instant starts\n            ? (now < starts ? OfferAvailability.NotStarted : OfferAvailability.ended(open, endsAt, now))\n            : OfferAvailability.ended(open, endsAt, now);\n\n    private static OfferAvailability ended(OfferAvailability open, Instant? endsAt, Instant now)\n        => endsAt is Instant ends ? (now >= ends ? OfferAvailability.Ended(ends) : open) : open;\n\n    law anOfferNotStartedComesBeforeItsEnd(Instant now, Instant starts, Instant ends, int claims)\n        when now < starts\n        => OfferAvailability.at(OfferAvailability.of(true, true, starts, ends, null, claims, null, null), now) is OfferAvailability.NotStarted;\n}\n";

/// A law over a machine's `Instant`s, compared through `Instant`'s `operator <=>` under the `if`s of the machine's code,
/// proves within its time, as it did before a regression.
#[test]
fn a_law_over_instants_a_machine_compares_under_its_ifs_proves_in_time() {
    let directory = library_project(&[
        ("app/Offers/Variant.sharp", "namespace App.Offers;\n\npublic interface Variant { }\n"),
        ("app/Offers/OfferAvailability.sharp", OFFER_AVAILABILITY),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Offers/OfferAvailability.sharp").exists());
}

/// `at`'s chain of `is` tests on `status` runs the arm of the first test that holds, so a sold-out offer with no
/// instants stays sold out, and a law that it becomes available is refused.
#[test]
fn a_false_law_over_a_chain_of_tests_on_one_value_is_not_proven() {
    let law = "law anOfferNotStartedComesBeforeItsEnd(Instant now, Instant starts, Instant ends, int claims)\n        when now < starts\n        => OfferAvailability.at(OfferAvailability.of(true, true, starts, ends, null, claims, null, null), now) is OfferAvailability.NotStarted;";
    let offer = OFFER_AVAILABILITY.replace(
        law,
        "law aSoldOutOfferBecomesAvailable(Instant now) => OfferAvailability.at(OfferAvailability.SoldOut(null, null), now) is OfferAvailability.Available;",
    );
    assert_ne!(offer, OFFER_AVAILABILITY, "the machine states the law the test replaces");
    let directory = library_project(&[
        ("app/Offers/Variant.sharp", "namespace App.Offers;\n\npublic interface Variant { }\n"),
        ("app/Offers/OfferAvailability.sharp", &offer),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains("error[law-unproven]: law aSoldOutOfferBecomesAvailable is not proven"),
        "{}",
        printed(&output)
    );
    assert!(!compiled(root, "app/Offers/OfferAvailability.sharp").exists());
}

/// `app/Plans/PlanState.sharp`, a machine of seven cases whose fields carry `[Key]`, with a move that gives one of two
/// cases through `?:`, and two predicates that `match (this)` whose last arm names each other case with `or`, the
/// second calling the first.
const PLAN_STATE: &str = "namespace App.Plans;\n\nimport Sharp.Key;\nimport Sharp.Tag;\nimport Sharp.Time.Instant;\n\n[Tag(\"state\")]\npublic machine PlanState\n{\n    [Key(\"draft\")]\n    case Draft\n    {\n        public PlanState.Live start(Instant chargeAt) => PlanState.Live(false, chargeAt);\n\n        public PlanState.Trial try(Instant chargeAt, Instant trialEndsAt) => PlanState.Trial(chargeAt, trialEndsAt);\n    }\n\n    [Key(\"trial\")]\n    case Trial([Key(\"charge_at\")] Instant chargeAt, [Key(\"trial_ends_at\")] Instant trialEndsAt)\n    {\n        public PlanState.Live convert() => PlanState.Live(false, this.chargeAt);\n    }\n\n    [Key(\"live\")]\n    case Live([Key(\"stops_at_end\")] bool stopsAtEnd, [Key(\"charge_at\")] Instant chargeAt)\n    {\n        public PlanState.Overdue|PlanState.Lapsed lapse(Instant retryAt, int maxTries, string why, Instant failedAt)\n            => PlanState.triesLeft(0, maxTries) ? PlanState.Overdue(1, retryAt, why, failedAt) : PlanState.Lapsed(failedAt);\n\n        public PlanState.Held hold(Instant? resumesAt) => PlanState.Held(resumesAt);\n\n        public PlanState.Closed close(Instant at) => PlanState.Closed(at);\n    }\n\n    [Key(\"held\")]\n    case Held([Key(\"resumes_at\")] Instant? resumesAt)\n    {\n        public PlanState.Live resume(Instant chargeAt) => PlanState.Live(false, chargeAt);\n    }\n\n    [Key(\"overdue\")]\n    case Overdue(\n        [Key(\"tries\")] int tries,\n        [Key(\"retry_at\")] Instant retryAt,\n        [Key(\"why\")] string why,\n        [Key(\"failed_at\")] Instant failedAt,\n    ) {\n        public PlanState.Live recover(Instant chargeAt) => PlanState.Live(false, chargeAt);\n\n        public PlanState.Lapsed lapse(Instant at) => PlanState.Lapsed(at);\n    }\n\n    [Key(\"lapsed\")]\n    case Lapsed([Key(\"lapsed_at\")] Instant lapsedAt)\n    {\n        public PlanState.Closed close(Instant at) => PlanState.Closed(at);\n    }\n\n    [Key(\"closed\")]\n    final case Closed([Key(\"closed_at\")] Instant closedAt);\n\n    public static PlanState.Draft enter() => PlanState.Draft;\n\n    public bool charges() => match (this) {\n        PlanState.Trial => true,\n        PlanState.Live live => !live.stopsAtEnd,\n        PlanState.Draft or PlanState.Held or PlanState.Overdue or PlanState.Lapsed or PlanState.Closed => false,\n    };\n\n    public bool chargeable() => match (this) {\n        PlanState.Overdue => true,\n        PlanState.Draft or PlanState.Trial or PlanState.Live or PlanState.Held or PlanState.Lapsed or PlanState.Closed => this.charges(),\n    };\n\n    law anOverduePlanIsChargeable(Instant chargeAt, Instant retryAt, string why, Instant failedAt)\n        => PlanState.enter().start(chargeAt).lapse(retryAt, 2, why, failedAt).chargeable();\n\n    public static bool triesLeft(int tries, int maxTries) => maxTries >= 1 && tries < maxTries - 1;\n}\n";

/// A law that walks a machine's entry point through two moves and ends in a predicate whose `match` names its last
/// cases with `or` proves within its time, as it did before a release wrote that arm again for each case an arm
/// before it left out, and ran out of time.
#[test]
fn a_law_over_a_match_whose_last_arm_names_each_other_case_with_or_proves_in_time() {
    let directory = library_project(&[("app/Plans/PlanState.sharp", PLAN_STATE)]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Plans/PlanState.sharp").exists());
}

/// Each case of the `or` arm runs that arm's branch, so a live plan that stops at its period's end charges nothing, and
/// a law that it is chargeable is refused with its claim still to show.
#[test]
fn a_false_law_over_a_match_whose_last_arm_names_each_other_case_with_or_is_not_proven() {
    let plan = PLAN_STATE.replace(
        "law anOverduePlanIsChargeable(Instant chargeAt, Instant retryAt, string why, Instant failedAt)\n        => PlanState.enter().start(chargeAt).lapse(retryAt, 2, why, failedAt).chargeable();",
        "law aStoppingPlanIsChargeable(Instant chargeAt) => PlanState.Live(true, chargeAt).chargeable();",
    );
    assert_ne!(plan, PLAN_STATE, "the machine states the law the test replaces");
    let directory = library_project(&[("app/Plans/PlanState.sharp", &plan)]);
    let root = directory.path();

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "error[law-unproven]: law aStoppingPlanIsChargeable is not proven\n = still to show: PlanState.Live(true, chargeAt).chargeable()\n"
        ),
        "{}",
        printed(&output)
    );
    assert!(!compiled(root, "app/Plans/PlanState.sharp").exists());
}

/// A struct's precondition holds what each value its fields hold keeps, so a law over a `Slot` reads that the
/// `Instant` it holds has a nanosecond in range.
#[test]
fn a_law_over_a_struct_reads_the_checks_of_the_instant_its_field_holds() {
    let directory = library_project(&[(
        "app/Shared/Slot.sharp",
        "namespace App.Shared;\n\nimport Sharp.Time.Instant;\n\npublic struct Slot\n{\n    public Slot(public Instant at { get; }) { }\n\n    law nanosecondInRange(Slot slot) => slot.at.nanosecond <= 999_999_999;\n}\n",
    )]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shared/Slot.sharp").exists());
}

/// A move starts from a case whose `Instant`s keep `Instant`'s checks, so an invariant that a clock's time never
/// falls behind its start holds after a move that adds a second to it.
#[test]
fn a_machine_s_invariant_over_its_case_s_instants_holds_after_a_move_that_adds_a_duration() {
    let directory = library_project(&[(
        "app/Shared/Clock.sharp",
        "namespace App.Shared;\n\nimport Sharp.Time.Duration;\nimport Sharp.Time.Instant;\n\npublic machine Clock\n{\n    case Ticking(Instant start, Instant now)\n    {\n        public Clock.Ticking tick() => Clock.Ticking(this.start, this.now.plus(Duration.ofSeconds(1)));\n    }\n\n    public static Clock.Ticking begin(Instant at) => Clock.Ticking(at, at);\n\n    law nowNeverBeforeStart(Clock.Ticking clock) => clock.now >= clock.start;\n}\n",
    )]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shared/Clock.sharp").exists());
}

/// A law's call of a law over an `Instant` shows that the `Instant` it passes keeps the checks of `Instant`'s
/// constructor, as it shows the called law's `when` condition: an `Instant` the calling law takes keeps them, and so
/// does one it builds, whatever nanosecond it builds it with.
#[test]
fn a_law_s_call_shows_that_each_instant_it_passes_keeps_instant_s_checks() {
    let directory = library_project(&[(
        "app/Shared/Calls.sharp",
        "namespace App.Shared;\n\nimport Sharp.Time.Instant;\n\npublic final class Calls\n{\n    law sameSecond(Instant at) => at.epochSeconds == at.epochSeconds;\n    law inRange(Instant at) => at.nanosecond <= 999_999_999;\n    law callsOnItsOwn(Instant at) => at.nanosecond < 1_000_000_000\n    {\n        Calls.inRange(at);\n    }\n    law callsOnBuilt(int s) => true\n    {\n        Calls.sameSecond(new Instant(s, 5));\n    }\n    law callsOnAnyBuilt(int s, int n) => true\n    {\n        Calls.sameSecond(new Instant(s, n));\n    }\n}\n",
    )]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shared/Calls.sharp").exists());
}

/// `App.Repro.OptionalCaseFieldRepro`, a machine whose `Held` case holds an id, with a static `none()` that gives its
/// `Empty` case, a method that returns a bound case's id or `null` with an `if`, one that does with `? :`, and `laws`.
fn optional_case_field(laws: &str) -> String {
    format!(
        "namespace App.Repro;\n\npublic machine OptionalCaseFieldRepro\n{{\n    case Held(int id)\n    {{\n        public OptionalCaseFieldRepro.Empty drop() => OptionalCaseFieldRepro.Empty;\n    }}\n\n    final case Empty;\n\n    public static OptionalCaseFieldRepro.Empty none() => OptionalCaseFieldRepro.Empty;\n\n    public static OptionalCaseFieldRepro.Held hold(int id) => OptionalCaseFieldRepro.Held(id);\n\n    public int? heldId()\n    {{\n        if (this is OptionalCaseFieldRepro.Held held) {{\n            return held.id;\n        }}\n\n        return null;\n    }}\n\n    public int? heldIdOrNull() => this is OptionalCaseFieldRepro.Held held ? held.id : null;\n\n    {laws}\n}}\n"
    )
}

/// A member's name never hides a name the law checker writes: beside a static `none()`, `null` is still `null`, so a
/// law reads a method that returns a bound case's field or `null`, written with an `if` or with `? :`.
#[test]
fn a_law_reads_a_case_s_field_or_null_beside_a_member_named_none() {
    let directory = lawful_project(&[(
        "app/Repro/OptionalCaseFieldRepro.sharp",
        &optional_case_field(
            "law noneHoldsNoId(int unused) => OptionalCaseFieldRepro.none().heldId() == null;\n    law noneHoldsNoIdOrNull(int unused) => OptionalCaseFieldRepro.none().heldIdOrNull() == null;\n    law heldHoldsItsId(int id) => OptionalCaseFieldRepro.Held(id).heldId() == id;",
        ),
    )]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Repro/OptionalCaseFieldRepro.sharp").exists());
}

/// A false law over a case's field or `null` beside a static `none()` is refused as not proven, with its claim still
/// to show, and never as a law the checker translated wrongly.
#[test]
fn a_false_law_over_a_case_s_field_or_null_is_not_proven() {
    let directory = lawful_project(&[(
        "app/Repro/OptionalCaseFieldRepro.sharp",
        &optional_case_field("law noneHoldsOne(int unused) => OptionalCaseFieldRepro.none().heldId() == 1;"),
    )]);
    let root = directory.path();

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains("error[law-unproven]: law noneHoldsOne is not proven\n = still to show: "),
        "{}",
        printed(&output)
    );
    assert!(!printed(&output).contains("translated the code it reaches wrongly"), "{}", printed(&output));
}

/// Lean opens a definition's namespace in its body, so a member of a class is in scope in each of the class's own
/// definitions. Each declaration a generated module writes, and each field and case of a type, is `protected`, so no
/// member of a class, a struct, an enum, an interface or a machine, whatever its name, hides a name the law checker
/// writes, such as `none`, `pure` or `Int`.
#[cfg(unix)]
#[test]
fn each_declaration_a_generated_module_writes_is_protected() {
    let directory = library_project(&[
        (
            "app/Delivery/DeliveryStatus.sharp",
            "namespace App.Delivery;\n\npublic sealed interface DeliveryStatus\n{\n}\n",
        ),
        (
            "app/Delivery/Pending.sharp",
            "namespace App.Delivery;\n\nimport Sharp.Time.Instant;\n\npublic final class Pending : DeliveryStatus\n{\n    public Pending(public string? error { get; } = null) { }\n\n    public static Pending failed(string error = \"timeout\") => new Pending(error);\n\n    public Done deliver(Instant at) => new Done(at);\n\n    law deliveredAt(Instant at) => new Pending().deliver(at).doneAt == at;\n    law failedTimesOut(int unused) => Pending.failed().error == \"timeout\";\n}\n",
        ),
        (
            "app/Delivery/Done.sharp",
            "namespace App.Delivery;\n\nimport Sharp.Time.Instant;\n\npublic final class Done : DeliveryStatus\n{\n    internal Done(public Instant doneAt { get; }) { }\n}\n",
        ),
        ("app/Runs/Phase.sharp", PHASE),
        ("app/Runs/Slot.sharp", SLOT),
        (
            "app/Runs/Worker.sharp",
            &worker(
                "law advancingMovesOn(Slot slot, int position) when position >= 0 && position < 100 => Worker.Running(slot, position).advance().position == position + 1;",
            ),
        ),
        (
            "app/Shared/Clock.sharp",
            "namespace App.Shared;\n\nimport Sharp.Time.Duration;\nimport Sharp.Time.Instant;\n\npublic machine Clock\n{\n    case Ticking(Instant start, Instant now)\n    {\n        public Clock.Ticking tick() => Clock.Ticking(this.start, this.now.plus(Duration.ofSeconds(1)));\n    }\n\n    public static Clock.Ticking begin(Instant at) => Clock.Ticking(at, at);\n\n    law nowNeverBeforeStart(Clock.Ticking clock) => clock.now >= clock.start;\n}\n",
        ),
        (
            "app/Repro/OptionalCaseFieldRepro.sharp",
            &optional_case_field("law noneHoldsNoId(int unused) => OptionalCaseFieldRepro.none().heldId() == null;"),
        ),
    ]);
    let root = directory.path();
    let lake = tempfile::tempdir().unwrap();

    let output = process::output(command(root, "never").env("XDG_CACHE_HOME", counting_cache(lake.path()))).unwrap();

    assert!(output.status.success(), "{}", printed(&output));
    let keywords = ["def ", "theorem ", "abbrev ", "instance ", "structure ", "inductive ", "opaque "];
    let mut open = Vec::new();
    for (module, text) in runner_request(lake.path()).modules {
        let mut cases = false;
        for line in text.lines() {
            let declared =
                line.strip_prefix("@[").and_then(|line| line.split_once("] ")).map_or(line, |(_, rest)| rest);
            let field = line.starts_with("  «") && line.contains(" : ");
            let case = cases && line.starts_with("  | ") && !line.starts_with("  | protected ");
            if keywords.iter().any(|keyword| declared.starts_with(keyword)) || field || case {
                open.push(format!("{module}: {line}"));
            }
            if declared.starts_with("protected inductive ") {
                cases = true;
            } else if !line.starts_with("  ") {
                cases = false;
            }
        }
    }
    assert_eq!(open, Vec::<String>::new());
}

/// `App.Classes.ClassOfRepro`, a machine whose `Open` case closes into its `Closed` case, with a static `closes` that
/// answers whether a move from one of its classes to another closes it, and `laws`.
fn class_of_repro(laws: &str) -> String {
    format!(
        "namespace App.Classes;\n\npublic machine ClassOfRepro\n{{\n    case Open\n    {{\n        public ClassOfRepro.Closed close() => ClassOfRepro.Closed;\n    }}\n\n    final case Closed;\n\n    public static ClassOfRepro.Open enter() => ClassOfRepro.Open;\n\n    public static bool closes(Class<ClassOfRepro> from, Class<ClassOfRepro> to) => from != to && to == typeof(ClassOfRepro.Closed);\n\n    {laws}\n}}\n"
    )
}

/// Spec section 25: a machine's classes are its cases, so a law reads `typeof` of a case, `Class.of` of a value, and
/// `==` on two classes, and a law over every pair of the machine's classes proves what a method taking two reads.
#[test]
fn a_law_reads_the_classes_of_a_machine_s_cases() {
    let directory = lawful_project(&[(
        "app/Classes/ClassOfRepro.sharp",
        &class_of_repro(
            "law closingCloses() => Class.of(ClassOfRepro.enter().close()) == typeof(ClassOfRepro.Closed);\n    law aClosedValueIsClosed(ClassOfRepro value) => value is ClassOfRepro.Closed ? Class.of(value) == typeof(ClassOfRepro.Closed) : true;\n    law closesOnlyFromOpen(Class<ClassOfRepro> from, Class<ClassOfRepro> to) => ClassOfRepro.closes(from, to) ? from == typeof(ClassOfRepro.Open) : true;",
        ),
    )]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Classes/ClassOfRepro.sharp").exists());
}

/// A false law over the classes of a machine's cases is refused as not proven, with its claim still to show, and with
/// each counterexample in PHP#, as the `typeof` of the case each class is.
#[test]
fn a_false_law_over_the_classes_of_a_machine_s_cases_is_not_proven() {
    let directory = lawful_project(&[(
        "app/Classes/ClassOfRepro.sharp",
        &class_of_repro(
            "law openIsNotClosed() => Class.of(ClassOfRepro.enter()) == typeof(ClassOfRepro.Closed);\n    law everyPairCloses(Class<ClassOfRepro> from, Class<ClassOfRepro> to) => ClassOfRepro.closes(from, to);",
        ),
    )]);
    let root = directory.path();

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "error[law-unproven]: law openIsNotClosed is not proven\n = still to show: Class.of(ClassOfRepro.enter()) == typeof(ClassOfRepro.Closed)\n"
        ),
        "{}",
        printed(&output)
    );
    assert!(
        printed(&output).contains(
            "error[law-unproven]: law everyPairCloses is not proven\n = when from == typeof(ClassOfRepro.Open) && to == typeof(ClassOfRepro.Open), still to show: ClassOfRepro.closes(from, to)\n"
        ),
        "{}",
        printed(&output)
    );
    assert!(!printed(&output).contains("translated the code it reaches wrongly"), "{}", printed(&output));
}

/// `App.Classes.Status`, a sealed interface whose implementers are `App.Classes.Waiting` and `App.Classes.Served`, each
/// file by its path.
const STATUSES: [(&str, &str); 3] = [
    ("app/Classes/Status.sharp", "namespace App.Classes;\n\npublic sealed interface Status\n{\n}\n"),
    (
        "app/Classes/Waiting.sharp",
        "namespace App.Classes;\n\npublic final class Waiting : Status\n{\n    public Waiting(public int ticket { get; }) { }\n}\n",
    ),
    (
        "app/Classes/Served.sharp",
        "namespace App.Classes;\n\npublic final class Served : Status\n{\n    public Served(public int ticket { get; }) { }\n}\n",
    ),
];

/// A false law over a sealed interface's values and classes names each counterexample in PHP#: a value as the
/// implementer it is, and a class as the `typeof` of that implementer.
#[test]
fn a_false_law_over_a_sealed_interface_names_each_implementer_as_php_sharp_writes_it() {
    let laws = (
        "app/Classes/Families.sharp",
        "namespace App.Classes;\n\npublic final class Families\n{\n    law everyStatusIsServed(Status status) => Class.of(status) == typeof(Served);\n    law everyStatusClassIsServed(Class<Status> kind) => kind == typeof(Served);\n}\n",
    );
    let directory = lawful_project(&[STATUSES.as_slice(), &[laws]].concat());
    let root = directory.path();

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "error[law-unproven]: law everyStatusIsServed is not proven\n = when status is Waiting, still to show: "
        ),
        "{}",
        printed(&output)
    );
    assert!(
        printed(&output).contains(
            "error[law-unproven]: law everyStatusClassIsServed is not proven\n = when kind == typeof(Waiting), still to show: "
        ),
        "{}",
        printed(&output)
    );
}

/// Spec section 25: the classes of an enum whose cases carry data are its cases, and the classes of a sealed
/// interface are its implementers, so a law reads `Class.of` of their values as it does a machine's.
#[test]
fn a_law_reads_the_classes_of_a_data_enum_and_of_a_sealed_interface() {
    let families = [
        (
            "app/Classes/Shape.sharp",
            "namespace App.Classes;\n\npublic enum Shape\n{\n    case Dot;\n    case Circle(int radius);\n}\n",
        ),
        (
            "app/Classes/Families.sharp",
            "namespace App.Classes;\n\npublic final class Families\n{\n    law aCircleIsACircle(int r) => Class.of(Shape.Circle(r)) == typeof(Shape.Circle);\n    law aDotIsNoCircle(int unused) => Class.of(Shape.Dot) != typeof(Shape.Circle);\n    law waitingIsWaiting(int t) => Class.of(new Waiting(t)) == typeof(Waiting);\n    law aStatusIsWaitingOrServed(Status status) => Class.of(status) == typeof(Waiting) || Class.of(status) == typeof(Served);\n}\n",
        ),
    ];
    let directory = lawful_project(&[STATUSES.as_slice(), families.as_slice()].concat());
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Classes/Families.sharp").exists());
}

/// A class outside any machine, enum or sealed interface has classes the running code alone tells apart, so a law that
/// reads `Class.of` of its value keeps its refusal.
#[test]
fn a_law_over_the_class_of_a_class_outside_any_family_is_refused() {
    let directory = lawful_project(&[(
        "app/Classes/Lamp.sharp",
        "namespace App.Classes;\n\npublic final class Lamp\n{\n    public Lamp(public int watts { get; }) { }\n\n    law aLampIsALamp(int watts) => Class.of(new Lamp(watts)) == typeof(Lamp);\n}\n",
    )]);
    let root = directory.path();

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "error[unmodeled-construct]: Law aLampIsALamp reaches the plain PHP class Class in Lamp.aLampIsALamp."
        ),
        "{}",
        printed(&output)
    );
    assert!(
        printed(&output)
            .contains("error[unmodeled-construct]: Law aLampIsALamp reaches a type test in Lamp.aLampIsALamp."),
        "{}",
        printed(&output)
    );
}

/// `App.Props.Light`, the machine of a reported problem: a computed `isOn` that matches on `this`, reached through each
/// case, and `laws`.
fn computed_light(laws: &str) -> String {
    format!(
        "namespace App.Props;\n\ninternal machine Light\n{{\n    case Off\n    {{\n        internal Light.On turnOn(int level) => Light.On(level);\n    }}\n\n    final case On(int level);\n\n    private static Light.Off enter() => Light.Off;\n\n    internal bool isOn => match (this) {{\n        Light.On => true,\n        Light.Off => false,\n    }};\n\n    {laws}\n}}\n"
    )
}

/// Spec section 6: a machine's computed property runs its body as a method of no arguments, which each case reads as
/// the machine's, so a law reads it through a case and an invariant of a case reads it too.
#[test]
fn a_law_reads_a_machine_s_computed_property_through_each_case() {
    let directory = lawful_project(&[(
        "app/Props/Light.sharp",
        &computed_light(
            "law onlyALitLightIsOn(int level) => Light.enter().turnOn(level).isOn && !Light.enter().isOn;\n    law anOnLightIsOn(Light.On light) => light.isOn;",
        ),
    )]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Props/Light.sharp").exists());
}

/// A false law over a machine's computed property is refused as not proven, with its claim still to show.
#[test]
fn a_false_law_over_a_machine_s_computed_property_is_not_proven() {
    let directory =
        lawful_project(&[("app/Props/Light.sharp", &computed_light("law offIsOn() => Light.enter().isOn;"))]);
    let root = directory.path();

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output)
            .contains("error[law-unproven]: law offIsOn is not proven\n = still to show: Light.enter().isOn\n"),
        "{}",
        printed(&output)
    );
    assert!(!printed(&output).contains("left out"), "{}", printed(&output));
}

/// Spec section 6: a plain class's computed property, written `=>` or with a `get` body, runs its body as a method of
/// no arguments, so a law reads it, and one that calls a method, and tells a stored `raw` from a computed `Raw`.
#[test]
fn a_law_reads_a_class_s_computed_property() {
    let directory = lawful_project(&[(
        "app/Props/Meter.sharp",
        "namespace App.Props;\n\npublic final class Meter\n{\n    public Meter(public int raw { get; }) { }\n\n    public int doubled => this.raw * 2;\n\n    public int Raw => this.raw * 2;\n\n    public int capped\n    {\n        get {\n            if (this.raw > 10) {\n                return 10;\n            }\n            return this.raw;\n        }\n    }\n\n    public int twice => this.helper();\n\n    public int helper() => this.raw + this.raw;\n\n    law doubledIsTwiceRaw(int raw) => new Meter(raw).doubled == raw * 2;\n    law upperRawIsDoubled(Meter m) => m.Raw == m.raw * 2;\n    law cappedAtTen(int raw) => new Meter(raw).capped <= 10;\n    law twiceCallsHelper(int raw) => new Meter(raw).twice == new Meter(raw).doubled;\n}\n",
    )]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Props/Meter.sharp").exists());
}

/// A subclass can override a `virtual` computed property, so a law reads it as each implementation gives it: it proves
/// what holds for every override, and its default body proves nothing more.
#[test]
fn a_law_reads_an_overridable_computed_property_as_each_implementation_gives_it() {
    let directory = lawful_project(&[(
        "app/Props/Gauge.sharp",
        "namespace App.Props;\n\npublic class Gauge\n{\n    public Gauge(public int raw { get; }) { }\n\n    public virtual int doubled => this.raw * 2;\n\n    law readsTheSame(Gauge g) => g.doubled == g.doubled;\n    law isTheDefault(Gauge g) => g.doubled == g.raw * 2;\n}\n",
    )]);
    let root = directory.path();

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(printed(&output).contains("error[law-unproven]: law isTheDefault is not proven\n"), "{}", printed(&output));
    assert!(!printed(&output).contains("readsTheSame"), "{}", printed(&output));
}

/// `App.Setup.Settings`, whose `problem` keeps a settings map's missing values with `filter`, tests what it kept with
/// `isNotEmpty()`, and names the first in a template with their count, and whose `active` has no problem, and `laws`.
fn settings(laws: &str) -> String {
    format!(
        "namespace App.Setup;\n\npublic final class Settings\n{{\n    public static string? problem(Map<string, string?> settings)\n    {{\n        const missing = settings.filter(value => value == null);\n        if (missing.isNotEmpty()) {{\n            return `Missing setting ${{missing.keys()[0]}}, ${{missing.count()}} in all`;\n        }}\n        return null;\n    }}\n\n    public static bool active(Map<string, string?> settings) => Settings.problem(settings) == null;\n\n    {laws}\n}}\n"
    )
}

/// A law reads a `Map`'s `filter` of a lambda, `isNotEmpty()` of what it kept, and a template of a `string` and an
/// `int`, so a settings map with a missing value is never active.
#[test]
fn a_law_reads_a_filtered_map_and_a_template_that_names_what_it_kept() {
    let directory = library_project(&[(
        "app/Setup/Settings.sharp",
        &settings(
            "law aMissingValueIsNeverActive(Map<string, string?> settings, string key) when settings.has(key) && settings.get(key) == null => !Settings.active(settings);",
        ),
    )]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Setup/Settings.sharp").exists());
}

/// A false law over a filtered map and a template is refused as not proven, with its claim still to show.
#[test]
fn a_false_law_over_a_filtered_map_is_not_proven() {
    let directory = library_project(&[(
        "app/Setup/Settings.sharp",
        &settings("law everySettingsMapIsActive(Map<string, string?> settings) => Settings.active(settings);"),
    )]);
    let root = directory.path();

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "error[law-unproven]: law everySettingsMapIsActive is not proven\n = still to show: Settings.problem(settings) == null\n"
        ),
        "{}",
        printed(&output)
    );
}

/// `isEmpty()` and `isNotEmpty()` of a `List` are `List.isEmpty` and its negation, so they prove what `count() == 0`
/// and `count() > 0` of the same list prove.
#[test]
fn a_list_s_is_empty_proves_what_its_count_proves() {
    let directory = library_project(&[(
        "app/Shared/Stock.sharp",
        "namespace App.Shared;\n\npublic final class Stock\n{\n    public static List<int> low(List<int> items) => items.filter(x => x < 5);\n\n    law emptyIsNoneCounted(List<int> items) => Stock.low(items).isEmpty() == (Stock.low(items).count() == 0);\n    law notEmptyIsSomeCounted(List<int> items) => Stock.low(items).isNotEmpty() == (Stock.low(items).count() > 0);\n}\n",
    )]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shared/Stock.sharp").exists());
}

/// A template part of a type other than `string` and `int`, as a `float`, keeps the template's refusal.
#[test]
fn a_law_reaching_a_template_with_a_float_part_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public static string show(int n) => `${n} of ${1.5}`;\n\n    law showsTheSame(int n) => Shape.show(n) == Shape.show(n);\n}\n",
        "Law showsTheSame reaches a string template in Shape.show. Join the strings with `+`.",
    );
}

/// `App.Setup.Requirement`, a class that declares its own `==`, by its name and whether it is met.
const REQUIREMENT: &str = "namespace App.Setup;\n\npublic final class Requirement\n{\n    public Requirement(public string name { get; }, public bool met { get; }) { }\n\n    public static bool operator ==(Requirement a, Requirement b) => a.name == b.name && a.met == b.met;\n\n    public int hash() => 0;\n}\n";

/// `App.Setup.Readiness`, which holds a `Map` of `Requirement`s that `active` keeps, and `laws`.
fn requirements(laws: &str) -> String {
    format!(
        "namespace App.Setup;\n\npublic final class Readiness\n{{\n    public Readiness(public Map<string, Requirement> requirements {{ get; }}) {{ }}\n\n    public static Readiness active(Map<string, Requirement> requirements) => new Readiness(requirements);\n\n    {laws}\n}}\n"
    )
}

/// Spec section 19: two `Map`s are `==` when they hold as many entries and each key of one is a key of the other with
/// an equal value, by the value's own `==`, so a law reads that a map a value keeps is the map it was given.
#[test]
fn a_law_compares_two_maps_by_their_values_own_equality() {
    let directory = library_project(&[
        ("app/Setup/Requirement.sharp", REQUIREMENT),
        (
            "app/Setup/Readiness.sharp",
            &requirements(
                "law keepsItsRequirements(Map<string, Requirement> requirements) => Readiness.active(requirements).requirements == requirements;",
            ),
        ),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Setup/Readiness.sharp").exists());
}

/// A false law over two `Map`s' `==` is refused as not proven, with its claim still to show.
#[test]
fn a_false_law_over_two_maps_equality_is_not_proven() {
    let directory = library_project(&[
        ("app/Setup/Requirement.sharp", REQUIREMENT),
        (
            "app/Setup/Readiness.sharp",
            &requirements("law anyTwoAgree(Map<string, Requirement> a, Map<string, Requirement> b) => a == b;"),
        ),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains("error[law-unproven]: law anyTwoAgree is not proven\n = still to show: a == b\n"),
        "{}",
        printed(&output)
    );
}

/// `App.Shared.Stamp`, whose laws build an `Instant`: one reads what it built, and one holds only because the
/// constructor throws for a nanosecond outside its range.
const STAMP: &str = "namespace App.Shared;\n\nimport Sharp.Time.Instant;\n\npublic final class Stamp\n{\n    law builds(int unused) => new Instant(1, 0).epochSeconds == 1;\n    law aBuiltNanosecondIsInRange(int s, int n) => new Instant(s, n).nanosecond <= 999_999_999;\n}\n";

/// `App.Shared.Deadline`, a machine whose law orders two `Instant`s, one of them a parameter named `until`.
const DEADLINE: &str = "namespace App.Shared;\n\nimport Sharp.Time.Instant;\n\npublic machine Deadline\n{\n    case Open(Instant until)\n    {\n        public Deadline.Closed close() => Deadline.Closed;\n    }\n\n    final case Closed;\n\n    public static Deadline.Open open(Instant until) => Deadline.Open(until);\n\n    law laterIsLater(Instant until, Instant now) when now >= until => !(now < until);\n}\n";

/// `App.Shared.SetupStatus`, a machine whose install expires 15 minutes after it starts, through `Instant.plus`.
const SETUP_STATUS: &str = "namespace App.Shared;\n\nimport Sharp.Time.Duration;\nimport Sharp.Time.Instant;\n\npublic machine SetupStatus\n{\n    case Installing(Instant startedAt)\n    {\n        public SetupStatus.Ready finish() => SetupStatus.Ready;\n    }\n\n    final case Ready;\n\n    public static SetupStatus.Installing start(Instant at) => SetupStatus.Installing(at);\n\n    public Instant? expiresAt() => this is SetupStatus.Installing i ? i.startedAt.plus(Duration.ofMinutes(15)) : null;\n\n    law anInstallExpiresAfterItsWindow(Instant at) => SetupStatus.Installing(at).expiresAt() == at.plus(Duration.ofMinutes(15));\n}\n";

/// Spec section 28: `Instant.toString()` writes its UTC text in PHP#, with no plain PHP and no zone's rules, so a law
/// reaches the text it gives and is refused only as not proven, never as reaching state or plain PHP.
#[test]
fn a_law_formats_an_instant_as_its_utc_text() {
    let directory = library_project(&[(
        "app/Shared/Stamp.sharp",
        "namespace App.Shared;\n\nimport Sharp.Time.Instant;\n\npublic final class Stamp\n{\n    law formats(int s, int n) => new Instant(s, n).toString() != \"\";\n}\n",
    )]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains("app/Shared/Stamp.sharp:7:5: error[law-unproven]: law formats is not proven\n"),
        "{}",
        printed(&output)
    );
    assert!(printed(&output).contains("error: found 1 issues"), "{}", printed(&output));
}

/// Spec section 28: a constructor whose body only checks its arguments and throws is a precondition of the value it
/// builds, so a law builds an `Instant`, relies on its nanosecond's range, orders two, and moves one with `plus`.
#[test]
fn a_law_builds_orders_and_moves_an_instant_through_its_constructor_s_checks() {
    let directory = library_project(&[
        ("app/Shared/Stamp.sharp", STAMP),
        ("app/Shared/Deadline.sharp", DEADLINE),
        ("app/Shared/SetupStatus.sharp", SETUP_STATUS),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    for file in ["app/Shared/Stamp.sharp", "app/Shared/Deadline.sharp", "app/Shared/SetupStatus.sharp"] {
        assert!(compiled(root, file).exists(), "{file}");
    }
}

/// Spec section 19: `<` on two values of a class that declares no `<=>` runs the one the sealed interface it implements
/// declares, as the checker chose it. A false order is not proven.
#[test]
fn compile_proves_a_law_that_orders_two_values_through_their_interface_s_comparison() {
    let priced = (
        "app/Shop/Priced.sharp",
        "namespace App.Shop;\n\npublic sealed interface Priced\n{\n    static int operator <=>(Priced a, Priced b) => match (a) {\n        Book first => match (b) {\n            Book second => first.cents <=> second.cents,\n        },\n    };\n}\n",
    );
    let book = |laws: &str| {
        format!(
            "namespace App.Shop;\n\npublic final class Book : Priced\n{{\n    public Book(public int cents {{ get; }}) {{ }}\n\n    {laws}\n}}\n"
        )
    };
    let books = book("law cheaperFirst(int low, int high) when low < high => new Book(low) < new Book(high);");
    let directory = lawful_project(&[priced, ("app/Shop/Book.sharp", &books)]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shop/Book.sharp").exists());

    let books = book("law dearerFirst(int low, int high) when low < high => new Book(high) < new Book(low);");
    let directory = lawful_project(&[priced, ("app/Shop/Book.sharp", &books)]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(printed(&output).contains("error[law-unproven]: law dearerFirst is not proven\n"), "{}", printed(&output));
}

/// Spec section 28: a standard library member a law can't reach is refused where the law reaches it, with the reason
/// project code gives, as `Instant.tryParse`, which calls plain PHP.
#[test]
fn a_law_reaching_a_standard_library_member_that_calls_plain_php_is_refused_naming_the_call() {
    assert_unmodeled_beside(
        "namespace App.Shared;\n\nimport Sharp.Time.Instant;\n\npublic class Shape\n{\n    law parses(string text) => Instant.tryParse(text) != null;\n}\n",
        &[],
        "Law parses reaches the plain PHP function `global.preg_match` in Instant.tryParse. Write the code the law reaches in PHP#, or pass the value plain PHP computes to the law as a parameter.",
    );
}

/// Spec section 28: a call in a law that leaves out an argument passes its parameter's default, as the call does at
/// run time, for a constructor, a static method and a method alike. `==` on two `Instant`s, a struct that declares no
/// `operator ==`, compares each field with the field's own `==`. A claim the default or the fields break is not proven.
#[test]
fn compile_proves_a_law_whose_calls_leave_out_defaulted_arguments() {
    let pending = |laws: &str| {
        format!(
            "namespace App.Delivery;\n\nimport Sharp.Time.Instant;\n\npublic final class Pending : DeliveryStatus\n{{\n    public Pending(public string? error {{ get; }} = null) {{ }}\n\n    public static Pending enqueued() => new Pending();\n\n    public static Pending failed(string error = \"timeout\") => new Pending(error);\n\n    public Pending postpone(string error = \"retry\") => new Pending(error);\n\n    public Done deliver(Instant at) => new Done(at);\n\n    {laws}\n}}\n"
        )
    };
    let family = |laws: &str| {
        library_project(&[
            (
                "app/Delivery/DeliveryStatus.sharp",
                "namespace App.Delivery;\n\npublic sealed interface DeliveryStatus\n{\n}\n",
            ),
            ("app/Delivery/Pending.sharp", &pending(laws)),
            (
                "app/Delivery/Done.sharp",
                "namespace App.Delivery;\n\nimport Sharp.Time.Instant;\n\npublic final class Done : DeliveryStatus\n{\n    internal Done(public Instant doneAt { get; }) { }\n}\n",
            ),
        ])
    };
    let directory = family(
        "law deliveredAt(Instant at) => new Pending().deliver(at).doneAt == at;\n    law enqueuedHoldsNoError(int unused) => Pending.enqueued().error == null;\n    law failedTimesOut(int unused) => Pending.failed().error == \"timeout\";\n    law postponedRetries(string? held) => new Pending(held).postpone().error == \"retry\";",
    );
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Delivery/Pending.sharp").exists());

    let directory = family(
        "law failedHoldsNothing(int unused) => Pending.failed().error == null;\n    law deliveredAtAnother(Instant at, Instant other) => new Pending().deliver(at).doneAt == other;",
    );

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    for law in ["failedHoldsNothing", "deliveredAtAnother"] {
        assert!(
            printed(&output).contains(&format!("error[law-unproven]: law {law} is not proven\n")),
            "{}",
            printed(&output)
        );
    }
}

/// Spec sections 19 and 28: the `==` a struct gets with no code compares a field that is a `List` of a class's values
/// element by element, by the class's own `==`, so a law that a struct equals itself proves.
#[test]
fn a_law_comparing_structs_whose_field_is_a_list_of_a_class_s_values_proves() {
    let directory = library_project(&[(
        "app/Shared/Shape.sharp",
        "namespace App.Shared;\n\nimport Sharp.Time.Instant;\n\npublic struct Shape\n{\n    public Shape(public List<Instant> closedAt { get; }) { }\n\n    law same(Shape shape) => shape == shape;\n}\n",
    )]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shared/Shape.sharp").exists());
}

/// Spec section 28: `==` and `!=` with a nullable value of a class run as spec section 19 lifts them: null equals only
/// null, and two values compare by the class's own `==`, whichever side is nullable. A false claim is not proven.
#[test]
fn a_law_compares_a_nullable_instant_with_eq() {
    let stamp = |laws: &str| {
        format!(
            "namespace App;\n\nimport Sharp.Time.Instant;\n\npublic struct Stamp\n{{\n    public Stamp(public Instant? at {{ get; }}) {{ }}\n\n    {laws}\n}}\n"
        )
    };
    let directory = library_project(&[(
        "app/Stamp.sharp",
        &stamp(
            "law keepsTheInstant(Instant at) => new Stamp(at).at == at;\n    law heldOnTheRight(Instant at) => at == new Stamp(at).at;\n    law keepsANullableInstant(Instant? at) => new Stamp(at).at == at;\n    law nullDiffersFromAnInstant(Instant at) => new Stamp(null).at != at;",
        ),
    )]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Stamp.sharp").exists());

    let directory = library_project(&[(
        "app/Stamp.sharp",
        &stamp("law nullEqualsAnInstant(Instant at) => new Stamp(null).at == at;"),
    )]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains("error[law-unproven]: law nullEqualsAnInstant is not proven\n"),
        "{}",
        printed(&output)
    );
}

/// Spec section 28: `==` on two values of a struct with an `Instant?` field compares that field as spec section 19 lifts
/// `==` on a nullable value. A false claim is not proven.
#[test]
fn a_law_compares_structs_whose_field_is_a_nullable_instant() {
    let shape = |laws: &str| {
        format!(
            "namespace App.Shared;\n\nimport Sharp.Time.Instant;\n\npublic struct Shape\n{{\n    public Shape(public Instant? closedAt {{ get; }}) {{ }}\n\n    {laws}\n}}\n"
        )
    };
    let directory = library_project(&[(
        "app/Shared/Shape.sharp",
        &shape(
            "law same(Shape shape) => shape == shape;\n    law openDiffersFromClosed(Instant at) => new Shape(null) != new Shape(at);",
        ),
    )]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shared/Shape.sharp").exists());

    let directory = library_project(&[(
        "app/Shared/Shape.sharp",
        &shape("law openEqualsClosed(Instant at) => new Shape(null) == new Shape(at);"),
    )]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains("error[law-unproven]: law openEqualsClosed is not proven\n"),
        "{}",
        printed(&output)
    );
}

/// Spec section 19: a struct's `==` compares each field with the field's own `==`, which no source applies: the `==` of
/// a struct that declares none, and the one a class declares. A claim either breaks is not proven.
#[test]
fn compile_proves_a_law_over_a_struct_whose_fields_compare_with_their_own_equality() {
    let point = (
        "app/Shared/Point.sharp",
        "namespace App.Shared;\n\npublic struct Point\n{\n    public Point(public int x { get; }, public int y { get; }) { }\n}\n",
    );
    let label = (
        "app/Shared/Label.sharp",
        "namespace App.Shared;\n\npublic final class Label\n{\n    public Label(public int id { get; }, public string text { get; }) { }\n\n    public static bool operator ==(Label a, Label b) => a.id == b.id;\n    public int hash() => this.id;\n}\n",
    );
    let pair = |law: &str| {
        format!(
            "namespace App.Shared;\n\npublic struct Pair\n{{\n    public Pair(public Point at {{ get; }}, public Label label {{ get; }}) {{ }}\n\n    {law}\n}}\n"
        )
    };
    let same = pair(
        "law sameById(int x, int id) => new Pair(new Point(x, 0), new Label(id, \"a\")) == new Pair(new Point(x, 0), new Label(id, \"b\"));",
    );
    let directory = lawful_project(&[point, label, ("app/Shared/Pair.sharp", &same)]);
    let root = directory.path();

    compile_succeeds(root);
    assert!(compiled(root, "app/Shared/Pair.sharp").exists());

    let differs = pair(
        "law samePoint(int x, int id) => new Pair(new Point(x, 0), new Label(id, \"a\")) == new Pair(new Point(x, 1), new Label(id, \"a\"));",
    );
    let directory = lawful_project(&[point, label, ("app/Shared/Pair.sharp", &differs)]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(printed(&output).contains("error[law-unproven]: law samePoint is not proven\n"), "{}", printed(&output));
}

/// Spec section 19: `==` on two values of a struct that holds another struct compares that field by its own `==`, and
/// `==` on two values of a class that declares `operator ==` runs it. A claim either breaks is not proven.
#[test]
fn compile_proves_a_law_over_a_struct_that_holds_a_struct_and_over_a_class_s_own_equality() {
    let window = (
        "app/Shared/Window.sharp",
        "namespace App.Shared;\n\nimport Sharp.Time.Instant;\n\npublic struct Window\n{\n    public Window(public Instant opened { get; }) { }\n}\n",
    );
    let badge = |laws: &str| {
        format!(
            "namespace App.Shared;\n\nimport Sharp.Time.Instant;\n\npublic final class Badge\n{{\n    public Badge(public int id {{ get; }}, public string label {{ get; }}) {{ }}\n\n    public static bool operator ==(Badge a, Badge b) => a.id == b.id;\n\n    public int hash() => this.id;\n\n    {laws}\n}}\n"
        )
    };
    let badges = badge(
        "law sameWindow(Instant at) => new Window(at) == new Window(at);\n    law sameBadge(int id) => new Badge(id, \"a\") == new Badge(id, \"b\");",
    );
    let directory = library_project(&[window, ("app/Shared/Badge.sharp", &badges)]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shared/Badge.sharp").exists());

    let badges = badge(
        "law anyWindow(Instant at, Instant other) => new Window(at) == new Window(other);\n    law differentBadges(int id) => new Badge(id, \"a\") != new Badge(id, \"b\");",
    );
    let directory = library_project(&[window, ("app/Shared/Badge.sharp", &badges)]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    for law in ["anyWindow", "differentBadges"] {
        assert!(
            printed(&output).contains(&format!("error[law-unproven]: law {law} is not proven\n")),
            "{}",
            printed(&output)
        );
    }
}

/// Spec section 28: a default a law can't read is refused, naming its parameter, only where a call leaves the argument
/// out, so a call that passes it still proves.
#[test]
fn a_law_that_leaves_out_an_argument_whose_default_a_law_cannot_read_is_refused_naming_the_parameter() {
    let shape = |law: &str| {
        format!(
            "namespace App.Shared;\n\npublic class Shape\n{{\n    public static int capped(int limit = 2 ** 3) => limit;\n\n    {law}\n}}\n"
        )
    };
    let directory = library_project(&[(
        "app/Shared/Shape.sharp",
        &shape("law cappedAtTen(int unused) => Shape.capped(10) == 10;"),
    )]);

    let output = compile(directory.path());

    assert!(output.status.success(), "{}", printed(&output));
    assert_unmodeled(
        &shape("law cappedAtMax(int unused) => Shape.capped() >= 0;"),
        "Law cappedAtMax reaches the operator `**` here in the default of `limit` in Shape.capped.",
    );
}

/// Spec section 28: a law reads the fields of an enum's data case, in the branch a pattern or a type test names the
/// case in, as the constructor's fields. A false claim over a field is not proven.
#[test]
fn compile_proves_a_law_over_the_fields_of_an_enum_s_data_case() {
    let result = |laws: &str| {
        format!(
            "namespace App.Shop;\n\npublic enum PaymentResult\n{{\n    case Paid(int amount);\n    case Declined(string reason);\n\n    public static PaymentResult charge(int amount) => amount > 0 ? PaymentResult.Paid(amount) : PaymentResult.Declined(\"empty\");\n\n    {laws}\n}}\n"
        )
    };
    let directory = lawful_project(&[(
        "app/Shop/PaymentResult.sharp",
        &result(
            "law chargeKeepsAmount(int amount) => PaymentResult.charge(amount) is PaymentResult.Paid paid ? paid.amount == amount : amount <= 0;\n    law declinedGivesReason(int amount) => match (PaymentResult.charge(amount)) {\n        PaymentResult.Paid(int paid) => paid > 0,\n        PaymentResult.Declined declined => declined.reason == \"empty\",\n    };",
        ),
    )]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shop/PaymentResult.sharp").exists());

    let directory = lawful_project(&[(
        "app/Shop/PaymentResult.sharp",
        &result(
            "law chargeDoubles(int amount) => PaymentResult.charge(amount) is PaymentResult.Paid paid ? paid.amount == amount * 2 : true;",
        ),
    )]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains("error[law-unproven]: law chargeDoubles is not proven\n"),
        "{}",
        printed(&output)
    );
}

/// Spec section 19: `==` on two values of an interface that extends `Equatable` holds when they are values of one
/// implementer and its own `==` holds. A law splits it on each implementer of a sealed one, and reads two values of an
/// open one as equal when they are one value, since it knows nothing else about them. A claim either breaks is not
/// proven.
#[test]
fn compile_proves_a_law_over_an_interface_that_extends_equatable() {
    let failures = |sealed: &str, laws: &str| {
        [
            (
                "app/Shop/Failure.sharp",
                format!("namespace App.Shop;\n\npublic {sealed}interface Failure : Equatable\n{{\n}}\n"),
            ),
            (
                "app/Shop/Declined.sharp",
                "namespace App.Shop;\n\npublic enum Declined : Failure\n{\n    case Card;\n    case Fraud;\n}\n"
                    .to_owned(),
            ),
            (
                "app/Shop/Lost.sharp",
                "namespace App.Shop;\n\npublic enum Lost : Failure\n{\n    case Gone;\n}\n".to_owned(),
            ),
            (
                "app/Shop/Failed.sharp",
                format!(
                    "namespace App.Shop;\n\npublic final class Failed\n{{\n    public Failed(public Failure failure {{ get; }}) {{ }}\n\n    public static Failure of(Failure failure) => failure;\n\n    {laws}\n}}\n"
                ),
            ),
        ]
    };
    let compiled_with = |sealed: &str, laws: &str| {
        let files = failures(sealed, laws);
        let files: Vec<(&str, &str)> = files.iter().map(|(name, source)| (*name, source.as_str())).collect();
        let directory = lawful_project(&files);
        let output = compile(directory.path());

        (directory, output)
    };
    let keeps = "law keepsItsFailure(Failure f) => new Failed(f).failure == f;";
    let apart = "law cardIsNoLoss(int unused) => Failed.of(Declined.Card) != Failed.of(Lost.Gone);";
    let card = "law aFailureIsItsCase(int unused) => Failed.of(Declined.Card) == Declined.Card;";

    for sealed in ["sealed ", ""] {
        let laws = if sealed.is_empty() { keeps.to_owned() } else { format!("{keeps}\n    {apart}\n    {card}") };
        let (directory, output) = compiled_with(sealed, &laws);

        assert!(output.status.success(), "{}", printed(&output));
        assert!(compiled(directory.path(), "app/Shop/Failed.sharp").exists());
    }

    for (sealed, law) in [
        ("sealed ", "law anyTwoAreOne(Failure a, Failure b) => a == b;"),
        ("sealed ", "law aFailureIsAnotherCase(int unused) => Failed.of(Declined.Card) == Declined.Fraud;"),
        ("", apart),
    ] {
        let (_directory, output) = compiled_with(sealed, law);
        let name = law.trim_start_matches("law ").split('(').next().unwrap();

        assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
        assert!(
            printed(&output).contains(&format!("error[law-unproven]: law {name} is not proven\n")),
            "{}",
            printed(&output)
        );
    }
}

/// A type test over an interface that is not sealed is refused: its implementers are open, so a law cannot tell them apart.
#[test]
fn a_law_reaching_a_type_test_over_an_open_interface_is_refused() {
    assert_unmodeled_beside(
        "namespace App.Shared;\n\npublic class Shape : Figure\n{\n    public static bool isShape(Figure figure) => figure is Shape;\n\n    law shapeIsShape(Shape s) => Shape.isShape(s);\n}\n",
        &[("app/Shared/Figure.sharp", "namespace App.Shared;\n\npublic interface Figure\n{\n}\n")],
        "Law shapeIsShape reaches a type test of the interface Figure in Shape.isShape. A law tells apart only the implementers of a sealed interface. Declare `Figure` sealed.",
    );
}

/// `mago analyze` refuses a law that reaches a construct the law translation does not model with the issue `mago
/// compile` refuses it with, once, though both its analysis and its compile refuse it, and compiles no file it refuses.
#[test]
fn analyze_refuses_a_law_that_reaches_an_unmodeled_construct_once_as_compile_does() {
    let directory = library_project(&[
        (
            "app/Shared/Shape.sharp",
            "namespace App.Shared;\n\npublic class Shape : Figure\n{\n    public static bool isShape(Figure figure) => figure is Shape;\n\n    law shapeIsShape(Shape s) => Shape.isShape(s);\n    law sameValue(int value) => value == value;\n}\n",
        ),
        ("app/Shared/Figure.sharp", "namespace App.Shared;\n\npublic interface Figure\n{\n}\n"),
    ]);
    let root = directory.path();
    let refusal = "Law shapeIsShape reaches a type test of the interface Figure in Shape.isShape. A law tells apart only the implementers of a sealed interface. Declare `Figure` sealed.";

    let analyzed = process::output(
        Command::new(env!("CARGO_BIN_EXE_mago"))
            .args(["--no-version-check", "--colors", "never", "analyze"])
            .env("MAGO_REPORTING_FORMAT", "medium")
            .env("XDG_CACHE_HOME", Path::new(env!("CARGO_TARGET_TMPDIR")).join("cache"))
            .current_dir(root),
    )
    .unwrap();

    assert_eq!(analyzed.status.code(), Some(1), "{}", printed(&analyzed));
    assert_eq!(
        printed(&analyzed)
            .matches(&format!("app/Shared/Shape.sharp:5:60: error[unmodeled-construct]: {refusal}"))
            .count(),
        1,
        "{}",
        printed(&analyzed)
    );
    assert!(!compiled(root, "app/Shared/Shape.sharp").exists(), "{}", printed(&analyzed));

    let compiled = compile_without_lean(root);

    assert!(
        printed(&compiled).contains(&format!("app/Shared/Shape.sharp:5:60:error - unmodeled-construct: {refusal}")),
        "{}",
        printed(&compiled)
    );
}

/// A law refusal is a result of analysis, and `mago analyze --scan-only` analyzes no method body, so it refuses no law,
/// where it once refused each law that reaches a method for having no body it could read.
#[test]
fn analyze_scan_only_refuses_no_law() {
    let directory = lawful_project(&[(
        "app/Level.sharp",
        "namespace App;\n\npublic final class Level\n{\n    public Level(public int rank { get; }) { }\n\n    public bool meets(Level other) => this.rank >= other.rank;\n\n    law meetsItself(Level a) => a.meets(a);\n}\n",
    )]);

    for arguments in [&["--scan-only"][..], &[]] {
        let analyzed = analyze(directory.path(), arguments);

        assert_eq!(analyzed.status.code(), Some(0), "{arguments:?}: {}", printed(&analyzed));
        assert!(printed(&analyzed).contains("No issues found."), "{arguments:?}: {}", printed(&analyzed));
    }
}

/// `mago analyze` translates each PHP# file a law reaches, a package's in `includes` too, as `mago compile` does, so a
/// law that reaches the standard library is refused for nothing, where it once was refused for errors no file had.
#[test]
fn analyze_proves_a_law_that_reaches_the_standard_library() {
    let directory = library_project(&[(
        "app/Stamp.sharp",
        "namespace App;\n\nimport Sharp.Time.Instant;\n\npublic struct Stamp\n{\n    public Stamp(public Instant at { get; }) { }\n\n    law keepsTheInstant(Instant at) => new Stamp(at).at.epochSeconds == at.epochSeconds && new Stamp(at).at.nanosecond == at.nanosecond;\n}\n",
    )]);

    let analyzed = analyze(directory.path(), &[]);

    assert_eq!(analyzed.status.code(), Some(0), "{}", printed(&analyzed));
    assert!(printed(&analyzed).contains("No issues found."), "{}", printed(&analyzed));
}

/// `mago analyze` refuses a law that reaches a file of the project whose analysis reported an error, for those errors.
#[test]
fn analyze_refuses_a_law_that_reaches_a_file_with_errors_for_its_errors() {
    let money = LAWFUL_MONEY
        .replace("    law addKeepsCurrency", "    public int broken() => \"text\";\n\n    law addKeepsCurrency");
    let directory = library_project(&[
        ("app/Shared/Money.sharp", &money),
        (
            "app/Shared/Ledger.sharp",
            "namespace App.Shared;\n\npublic class Ledger\n{\n    law addKeeps(Money a, Money b) => a.add(b).currency == a.currency;\n}\n",
        ),
    ]);

    let analyzed = analyze(directory.path(), &[]);

    assert_eq!(analyzed.status.code(), Some(1), "{}", printed(&analyzed));
    assert!(
        printed(&analyzed).contains(
            "app/Shared/Ledger.sharp:5:5: error[law-unproven]: Law addKeeps is not proven: it reaches Money.add, whose file has errors. Fix them, then run vendor/bin/mago compile again."
        ),
        "{}",
        printed(&analyzed)
    );
}

/// `mago analyze` refuses a law that reaches a package's PHP# file with an error for that file's errors, as `mago
/// compile` does. An analyze of the project's files reports no issue of the package's own, and an analyze of the whole
/// project, which compiles it, reports the compile's refusal of the package's file once.
#[test]
fn analyze_refuses_a_law_that_reaches_a_package_file_with_errors_for_its_errors() {
    let money = LAWFUL_MONEY
        .replace("namespace App.Shared;", "namespace Acme;")
        .replace("    law addKeepsCurrency", "    public int broken() => \"text\";\n\n    law addKeepsCurrency");
    let directory = lawful_project(&[
        (
            "vendor/acme/money/composer.json",
            "{\n    \"autoload\": {\n        \"psr-4\": {\n            \"Acme\\\\\": \"src/\"\n        }\n    }\n}\n",
        ),
        ("vendor/acme/money/src/Money.sharp", &money),
        (
            "app/Shared/Ledger.sharp",
            "namespace App.Shared;\n\nimport Acme.Money;\n\npublic class Ledger\n{\n    law addKeeps(Money a, Money b) => a.add(b).currency == a.currency;\n}\n",
        ),
    ]);

    let named = analyze(directory.path(), &["app"]);
    let whole = analyze(directory.path(), &[]);
    let refused = "app/Shared/Ledger.sharp:7:5: error[law-unproven]: Law addKeeps is not proven: it reaches Money.add, whose file has errors. Fix them, then run vendor/bin/mago compile again.";

    assert_eq!(named.status.code(), Some(1), "{}", printed(&named));
    assert!(printed(&named).contains(refused), "{}", printed(&named));
    assert!(!printed(&named).contains("Money.sharp"), "{}", printed(&named));
    assert_eq!(whole.status.code(), Some(1), "{}", printed(&whole));
    for issue in [
        refused,
        "vendor/acme/money/src/Money.sharp:8:28: error[invalid-return-statement]",
        "vendor/acme/money/src/Money.sharp comes from the Composer package acme/money.",
    ] {
        assert_eq!(printed(&whole).matches(issue).count(), 1, "{issue}\n{}", printed(&whole));
    }
}

/// Spec section 28's `app/Tenant/Store/Module.sharp`, whose law spans `Receipts` and `Refunds`.
const STORE_MODULE: &str = "namespace App.Tenant.Store;\n\nmodule\n{\n    law receiptMatchesRefunds(int paid, int refunded) => Receipts.refundable(paid, refunded) == Refunds.remaining(paid, refunded);\n}\n";

/// `Receipts` and `Refunds`, each with the method `STORE_MODULE`'s law reaches, written with `refundable`'s body.
fn store(refundable: &str, remaining: &str) -> tempfile::TempDir {
    lawful_project(&[
        ("app/Tenant/Store/Module.sharp", STORE_MODULE),
        (
            "app/Tenant/Store/Receipts.sharp",
            &format!(
                "namespace App.Tenant.Store;\n\npublic static class Receipts\n{{\n    public static int refundable(int paid, int refunded) => {refundable};\n}}\n"
            ),
        ),
        (
            "app/Tenant/Store/Refunds.sharp",
            &format!(
                "namespace App.Tenant.Store;\n\npublic static class Refunds\n{{\n    public static int remaining(int paid, int refunded) => {remaining};\n}}\n"
            ),
        ),
    ])
}

/// Spec section 28: the checker proves the module law from `Module.sharp` alone, and writes no `Module.lean`.
#[test]
fn compile_proves_spec_section_28_s_module_law_from_module_sharp_alone() {
    let directory = store("refunded < paid ? refunded : paid", "refunded < paid ? refunded : paid");
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert_eq!(source_lean_files(root), Vec::<PathBuf>::new());
    assert!(compiled(root, "app/Tenant/Store/Module.sharp").exists());
}

/// Spec section 28: a module law's body proves it as a class law's does, calling the laws of the module and of its
/// classes through their names.
#[test]
fn compile_proves_a_module_law_by_its_body() {
    let module = "namespace App.Tenant.Store;\n\nmodule\n{\n    law receiptMatchesRefunds(int paid, int refunded) => Receipts.refundable(paid, refunded) == Refunds.remaining(paid, refunded);\n    law receiptNeverAbovePaid(int paid, int refunded) when paid >= 0 && refunded >= 0 => Refunds.remaining(paid, refunded) <= paid\n    {\n        Module.receiptMatchesRefunds(paid, refunded);\n        Receipts.neverAbovePaid(paid, refunded);\n    }\n}\n";
    let receipts = "namespace App.Tenant.Store;\n\npublic static class Receipts\n{\n    public static int refundable(int paid, int refunded) => refunded < paid ? paid - refunded : 0;\n\n    law neverAbovePaid(int paid, int refunded) when paid >= 0 && refunded >= 0 => Receipts.refundable(paid, refunded) <= paid;\n}\n";
    let refunds = "namespace App.Tenant.Store;\n\npublic static class Refunds\n{\n    public static int remaining(int paid, int refunded) => refunded < paid ? paid - refunded : 0;\n}\n";
    let directory = lawful_project(&[
        ("app/Tenant/Store/Module.sharp", module),
        ("app/Tenant/Store/Receipts.sharp", receipts),
        ("app/Tenant/Store/Refunds.sharp", refunds),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert_eq!(source_lean_files(root), Vec::<PathBuf>::new());
    assert!(compiled(root, "app/Tenant/Store/Module.sharp").exists());
}

/// A module law that does not hold refuses `Module.sharp` and names the law, as a class law refuses its class's file,
/// and the classes it reaches still compile.
#[test]
fn compile_refuses_module_sharp_when_its_law_does_not_hold() {
    let directory = store("refunded < paid ? refunded : paid", "refunded < paid ? paid : refunded");
    let root = directory.path();

    let output = compile_lines(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "app/Tenant/Store/Module.sharp:5:5:error - law-unproven: law receiptMatchesRefunds is not proven"
        ),
        "{}",
        printed(&output)
    );
    assert!(!compiled(root, "app/Tenant/Store/Module.sharp").exists());
    assert!(compiled(root, "app/Tenant/Store/Receipts.sharp").exists());
    assert!(compiled(root, "app/Tenant/Store/Refunds.sharp").exists());
}

/// Spec section 32: a namespace's module is the one its folder's `Module.sharp` declares, and only it states laws. A
/// misplaced `module` in another file of the folder is refused on that file alone, and `Module.sharp` still compiles
/// and proves its law.
#[test]
fn compile_refuses_a_module_outside_module_sharp_on_its_own_file_and_proves_module_sharp() {
    for (other, code, refusal) in [
        (
            "app/Shop/CheckoutTest.sharp",
            "namespace App.Shop;\n\nmodule : App.Shop\n{\n    law same(int a) => a == a;\n}\n\npublic class CheckoutTest { }\n",
            "app/Shop/CheckoutTest.sharp:5:5:error - semantics: A module that overrides another states no laws: state it in its folder's Module.sharp.",
        ),
        (
            "app/Shop/CheckoutTest.sharp",
            "namespace App.Shop;\n\nmodule\n{\n}\n",
            "app/Shop/CheckoutTest.sharp:3:1:error - semantics: A module is declared in its folder's Module.sharp.",
        ),
        (
            "app/Shop/Module.Production.sharp",
            "namespace App.Shop;\n\nmodule\n{\n}\n",
            "app/Shop/Module.Production.sharp:3:1:error - semantics: An environment module overrides its folder's module: write `module : App.Shop`.",
        ),
    ] {
        let directory = lawful_project(&[("app/Shop/Module.sharp", SHOP_MODULE), (other, code)]);
        let root = directory.path();

        let output = compile_lines(root);

        assert_eq!(output.status.code(), Some(1), "{other}: {}", printed(&output));
        assert!(printed(&output).contains(refusal), "{other}: {}", printed(&output));
        assert!(!printed(&output).contains("app/Shop/Module.sharp"), "{other}: {}", printed(&output));
        assert!(compiled(root, "app/Shop/Module.sharp").exists(), "{other}");
    }
}

#[test]
fn compile_proves_a_law_over_locals_an_if_a_nullable_and_strings() {
    assert_proven(
        "Label",
        "namespace App.Shared;\n\npublic class Label\n{\n    public static string tag(string? name, bool loud)\n    {\n        let text = name ?? \"item\";\n        if (loud) {\n            text = text + \"!\";\n        }\n        return text;\n    }\n\n    law quietKeepsName(string name) => Label.tag(name, false) == name;\n}\n",
        "law loudKeepsName(string name) => Label.tag(name, true) == name;",
    );
}

/// A package's law is proven from its `.sharp` file as a law of the project is, and the compile writes nothing under
/// `vendor/`.
#[test]
fn compile_proves_a_package_law_and_writes_nothing_under_vendor() {
    let directory = lawful_project(&[
        (
            "vendor/acme/money/composer.json",
            "{\n    \"autoload\": {\n        \"psr-4\": {\n            \"Acme\\\\\": \"src/\"\n        }\n    }\n}\n",
        ),
        ("vendor/acme/money/src/Money.sharp", &LAWFUL_MONEY.replace("namespace App.Shared;", "namespace Acme;")),
    ]);
    let root = directory.path();
    let vendor = files_under(&root.join("vendor"));

    let output = compile_lines(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "vendor/acme/money/src/Money.sharp").exists());
    assert_eq!(files_under(&root.join("vendor")), vendor, "nothing is written under vendor/");
}

#[test]
fn compile_refuses_an_unproven_law_whatever_ignore_entry_or_baseline_entry_targets_it() {
    let broken =
        LAWFUL_MONEY.replace("this.amount + other.amount, this.currency", "this.amount + other.amount, other.currency");
    let workspace = |analyzer: &str| {
        let directory = lawful_project(&[("app/Shared/Money.sharp", &broken)]);
        write(
            directory.path(),
            "mago.toml",
            &format!("php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\n\n[analyzer]\n{analyzer}\n"),
        );
        directory
    };
    let baselined = workspace("baseline = \"baseline.toml\"");
    write(
        baselined.path(),
        "baseline.toml",
        "variant = \"strict\"\n\n[[entries.\"app/Shared/Money.sharp\".issues]]\ncode = \"law-unproven\"\nstart_line = 8\nend_line = 8\n",
    );

    for (suppression, directory) in
        [("an ignore entry", workspace("ignore = [\"law-unproven\"]")), ("a baseline entry", baselined)]
    {
        let output = compile_lines(directory.path());

        assert_eq!(output.status.code(), Some(1), "{suppression}: {}", printed(&output));
        assert!(
            printed(&output).contains("error - law-unproven: law addKeepsCurrency"),
            "{suppression}: {}",
            printed(&output)
        );
        assert!(!compiled(directory.path(), "app/Shared/Money.sharp").exists(), "{suppression}");
    }
}

/// Compiles `source` as `app/Shared/Shape.sharp`, in a project that installs the standard library and so the members of
/// a `List`, and checks that the compile refuses it with the unmodeled-construct error `message`, before any Lean
/// process starts.
fn assert_unmodeled(source: &str, message: &str) {
    assert_unmodeled_beside(source, &[], message);
}

/// [`assert_unmodeled`] with the project's `others` files, each a path and its code, beside `app/Shared/Shape.sharp`.
fn assert_unmodeled_beside(source: &str, others: &[(&str, &str)], message: &str) {
    let files: Vec<(&str, &str)> =
        [("app/Shared/Shape.sharp", source)].into_iter().chain(others.iter().copied()).collect();
    let directory = library_project(&files);
    let root = directory.path();

    let output = compile_without_lean(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(printed(&output).contains(&format!("error - unmodeled-construct: {message}")), "{}", printed(&output));
    assert!(!compiled(root, "app/Shared/Shape.sharp").exists());
}

#[test]
fn a_law_reaching_a_float_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public Shape(public int cents { get; }) { }\n    public float ratio() => this.cents / 2.0;\n\n    law ratioBelowOne(Shape p) => p.ratio() < 100.0;\n}\n",
        "Law ratioBelowOne reaches a float in Shape.ratio. A law cannot reason about float arithmetic, which rounds; use int.",
    );
}

/// A law reaches an instance method no subclass can override: a private one, and any method of an enum. A `virtual`
/// method of a class a subclass can extend is refused, since a law cannot know which implementation runs, and so is a
/// call through `Self`, even of an enum's private method.
#[test]
fn a_law_reaches_a_method_only_when_no_subclass_can_override_it() {
    let directory = lawful_project(&[
        (
            "app/Auth/Level.sharp",
            "namespace App.Auth;\n\npublic enum Level\n{\n    case Low;\n    case High;\n\n    private int rank() => match (this) {\n        Level.Low => 1,\n        Level.High => 2,\n    };\n    public bool meets(Level @required) => this.rank() >= @required.rank();\n\n    law meetsItself(Level a) => a.meets(a);\n}\n",
        ),
        (
            "app/Shared/Shape.sharp",
            "namespace App.Shared;\n\npublic class Shape\n{\n    public Shape(public int corners { get; }) { }\n    private int sides() => this.corners + 1;\n\n    law hasSides(Shape s) => s.corners < 0 || s.sides() > 0;\n}\n",
        ),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Auth/Level.sharp").exists());
    assert!(compiled(root, "app/Shared/Shape.sharp").exists());

    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public virtual int sides() => 3;\n\n    law hasSides(Shape s) => s.sides() > 0;\n}\n",
        "Law hasSides reaches a call to the overridable method Shape.sides in Shape.hasSides. A law cannot know which implementation runs.",
    );
    assert_unmodeled(
        "namespace App.Shared;\n\npublic enum Shape\n{\n    case Low;\n    case High;\n\n    private int rank() => match (this) {\n        Shape.Low => 1,\n        Shape.High => 2,\n    };\n    public int twice() => Self.rank() * 2;\n\n    law twiceIsPositive(Shape s) => s.twice() > 0;\n}\n",
        "Law twiceIsPositive reaches a call through `Self` in Shape.twice. A law cannot know which implementation runs.",
    );
}

#[test]
fn a_law_testing_the_identity_of_objects_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    law same(Shape a, Shape b) => a === b;\n}\n",
        "Law same reaches the identity test `===` in Shape.same. A law sees an object as its values, so it has no identity to compare.",
    );
}

/// Spec section 19: `==` on two Lists runs the elements' class operator `==` on each pair, as the bridge lowers it, so
/// a law that two equal lists hold equal amounts proves, and one that they hold equal currencies is not proven, since
/// two amounts match when the currencies differ. Lean's own `==` on Lists would compare every value the objects hold.
#[test]
fn a_law_comparing_lists_of_objects_runs_the_class_operator_on_each_pair() {
    let shape = |law: &str| {
        format!(
            "namespace App.Shared;\n\npublic class Shape\n{{\n    public Shape(public int amount {{ get; }}, public string currency {{ get; }}) {{ }}\n    public static bool operator ==(Shape a, Shape b) => a.amount == b.amount;\n    public int hash() => this.amount;\n\n    {law}\n}}\n"
        )
    };
    let directory = library_project(&[(
        "app/Shared/Shape.sharp",
        &shape("law sameListSameAmount(Shape a, Shape b) => !([a] == [b]) || a.amount == b.amount;"),
    )]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shared/Shape.sharp").exists());

    let directory = library_project(&[(
        "app/Shared/Shape.sharp",
        &shape("law sameListSameCurrency(Shape a, Shape b) => !([a] == [b]) || a.currency == b.currency;"),
    )]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains("error[law-unproven]: law sameListSameCurrency is not proven\n"),
        "{}",
        printed(&output)
    );
}

#[test]
fn a_law_reaching_a_try_catch_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\nimport DivisionByZeroError;\n\npublic class Shape\n{\n    public static int share(int a)\n    {\n        try {\n            return 10 / a;\n        } catch (DivisionByZeroError e) {\n            return 0;\n        }\n    }\n\n    law shared(int a) => Shape.share(a) >= 0;\n}\n",
        "Law shared reaches a try/catch in Shape.share. Return a value that tells the failure apart, such as `null` or an enum case, in place of the exception.",
    );
}

#[test]
fn a_law_reaching_a_type_test_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public static bool whole(Any? value) => value is int;\n\n    law wholeInt(int a) => Shape.whole(a);\n}\n",
        "Law wholeInt reaches Any in Shape.whole. It reads types while the code runs.",
    );
}

#[test]
fn a_law_reaching_a_member_lean_reserves_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public int rec() => 1;\n\n    law one(Shape s) => s.rec() == 1;\n}\n",
        "Law one reaches the method Shape.rec in Shape.rec. The checker gives every class a member of that name, so a law cannot reach a member that has it.",
    );
}

/// Spec section 28: a law reads a `Map` with the members whose model the checker has, and a member it has none of is
/// refused with those members named.
#[test]
fn a_law_reading_a_map_member_it_has_no_model_of_is_refused_naming_the_members_it_reads() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    law sized(Map<string, int> items) => items.toList().count() == items.count();\n}\n",
        "Law sized reaches the Map method toList in Shape.sized. Read a Map with `count()`, `isEmpty()`, `isNotEmpty()`, `get`, `has`, `keys()`, `values()`, `filter`, `any`, an index or a loop over its entries.",
    );
}

/// `App.Billing.Limit`, the limits a plan bounds.
const LIMIT: &str = "namespace App.Billing;\n\npublic enum Limit\n{\n    case Seats;\n    case Projects;\n}\n";

/// `App.Billing.Plan`, whose `limits()` gives each plan's `Map<Limit, int>`, `free` the free plan's entries, with laws
/// that read it with `get`, `has`, an index, `count()`, `keys()`, `values()` and a loop over its entries.
fn plan(free: &str) -> String {
    format!(
        "namespace App.Billing;\n\npublic enum Plan\n{{\n    case Free;\n    case Pro;\n\n    public Map<Limit, int> limits() => match (this) {{\n        Plan.Free => [{free}],\n        Plan.Pro => [Limit.Seats: 10, Limit.Projects: 100],\n    }};\n\n    public int total()\n    {{\n        let sum = 0;\n        for (const [limit, bound] of this.limits()) {{\n            sum += bound;\n        }}\n        return sum;\n    }}\n\n    law everyPlanBoundsEveryLimit(Plan plan, Limit limit) => (plan.limits().get(limit) ?? 0) >= 1;\n    law everyPlanNamesEveryLimit(Plan plan) => plan.limits().has(Limit.Seats) && (plan.limits()[Limit.Projects] ?? 0) >= 3 && plan.limits().count() == 2;\n    law everyLimitHasOneBound(Plan plan) => plan.limits().keys().count() == plan.limits().values().count();\n    law everyPlanAllowsSome(Plan plan) => plan.total() >= 2;\n}}\n"
    )
}

/// `App.Billing.Quota`, which reads a `Map<string, int>` a literal and a spread build.
const QUOTA: &str = "namespace App.Billing;\n\npublic class Quota\n{\n    public static int limit(Map<string, int> limits) => limits[\"orders\"] ?? 0;\n\n    public static Map<string, int> defaults() => [\"orders\": 3, \"seats\": 1];\n\n    law limited(int unused) => Quota.limit(Quota.defaults()) == 3 && Quota.limit([...Quota.defaults(), \"orders\": 5]) == 5;\n}\n";

/// Spec sections 12 and 28: a law reads a `Map`'s lookups, keys, values, entries and count, so it proves an invariant
/// over keyed data, as every plan bounding every limit, and a literal's later key replaces an earlier one's value. A
/// plan whose limit is 0 breaks the invariant, which is not proven.
#[test]
fn a_law_reads_a_map_s_lookups_keys_values_entries_and_count() {
    let directory = library_project(&[
        ("app/Billing/Limit.sharp", LIMIT),
        ("app/Billing/Plan.sharp", &plan("Limit.Seats: 1, Limit.Projects: 3")),
        ("app/Billing/Quota.sharp", QUOTA),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Billing/Plan.sharp").exists());
    assert!(compiled(root, "app/Billing/Quota.sharp").exists());

    let directory = library_project(&[
        ("app/Billing/Limit.sharp", LIMIT),
        ("app/Billing/Plan.sharp", &plan("Limit.Seats: 1, Limit.Projects: 0")),
    ]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains("error[law-unproven]: law everyPlanBoundsEveryLimit is not proven\n"),
        "{}",
        printed(&output)
    );
}

/// Spec section 20.2: a law about a machine whose case holds a `Map` proves, as one whose entry point builds the case
/// with `[:]` and whose move carries the `Map` on.
#[test]
fn a_law_proves_over_a_machine_whose_case_holds_a_map() {
    let directory = library_project(&[(
        "app/Run.sharp",
        "namespace App;\n\npublic machine Run\n{\n    case Running(Map<string, string> args, int position)\n    {\n        public Run.Running step() => Run.Running(this.args, this.position + 1);\n    }\n\n    public static Run.Running start() => Run.Running([:], 0);\n\n    law stepping(Run.Running r) => r.step().position == r.position + 1;\n}\n",
    )]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Run.sharp").exists());
}

/// `App.Runs.Mark`, a struct of one int.
const MARK: &str = "namespace App.Runs;\n\npublic struct Mark\n{\n    public Mark(public int at { get; }) { }\n}\n";

/// `App.Runs.Cursor`, a struct that holds a `Map` beside an `int?` and a `List<Mark>`.
const CURSOR: &str = "namespace App.Runs;\n\npublic struct Cursor\n{\n    public Cursor(public int? step { get; }, public Map<string, int> arguments { get; }, public List<Mark> marks { get; }) { }\n}\n";

/// `App.Runs.Trail`, a struct whose one field is a `List<Mark>`, so its `==` compares the lists.
const TRAIL: &str =
    "namespace App.Runs;\n\npublic struct Trail\n{\n    public Trail(public List<Mark> marks { get; }) { }\n}\n";

/// `App.Runs.RunStatus`, whose cases hold a `Cursor`, with laws that build one with `[:]`, take one as a parameter and
/// read its other properties, ask its `Map` `isEmpty()` and `isNotEmpty()`, and compare a `List<Mark>`, and a `Trail`
/// that holds one, with `==`.
const RUN_STATUS: &str = "namespace App.Runs;\n\npublic machine RunStatus\n{\n    case Running(Cursor cursor, int position)\n    {\n        public RunStatus.Waiting wait() => RunStatus.Waiting(this.cursor, this.position);\n\n        public RunStatus.Running restart(int step) => RunStatus.Running(new Cursor(step, [:], this.cursor.marks), this.position);\n    }\n\n    final case Waiting(Cursor cursor, int position);\n\n    public static RunStatus.Running start() => RunStatus.Running(new Cursor(null, [:], []), 0);\n\n    law waitingKeepsThePosition() => RunStatus.start().wait().position == 0;\n    law aFreshCursorHasNoArguments() => new Cursor(null, [:], []).arguments.isEmpty();\n    law aStepGivesNoArguments(int step) => !new Cursor(step, [:], []).arguments.isNotEmpty();\n    law marksCompare(List<Mark> marks) => marks == marks;\n    law trailsCompare(Trail trail) => trail == trail;\n    law aStepIsItsOwn(Cursor cursor) => cursor.step is int s ? s == s : cursor.marks.count() >= 0;\n    law restartingKeepsThePosition(RunStatus.Running r, int step) => r.restart(step).position == r.position;\n    law restartingSetsTheStep(RunStatus.Running r, int step) => r.restart(step).cursor.step == step;\n}\n";

/// Spec sections 12, 19 and 28: a value that holds a `Map`, built with `[:]` or taken as a parameter, translates to the
/// model, so a law reads its other properties, and its `Map`'s `isEmpty()` and `isNotEmpty()`. Two `List`s are `==`
/// when they hold as many elements and each pair is, by the element's own `==`, and so are two structs whose field is
/// a `List`.
#[test]
fn a_law_builds_takes_and_reads_a_struct_that_holds_a_map() {
    let directory = library_project(&[
        ("app/Runs/Mark.sharp", MARK),
        ("app/Runs/Cursor.sharp", CURSOR),
        ("app/Runs/Trail.sharp", TRAIL),
        ("app/Runs/RunStatus.sharp", RUN_STATUS),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Runs/RunStatus.sharp").exists());
}

/// `App.Runs.Prepared`, whose `Running` case holds a `Cursor`, with `prepare`, which takes a `Map` and builds the cursor
/// from it, `keep`, which takes none and copies every field, and `laws`.
fn prepared(laws: &str) -> String {
    format!(
        "namespace App.Runs;\n\npublic machine Prepared\n{{\n    case Running(Cursor cursor, int position)\n    {{\n        public Prepared.Running prepare(Map<string, int> arguments) => Prepared.Running(new Cursor(this.cursor.step, arguments, this.cursor.marks), this.position);\n\n        public Prepared.Running keep() => Prepared.Running(new Cursor(this.cursor.step, this.cursor.arguments, this.cursor.marks), this.position);\n    }}\n\n    public static Prepared.Running start(Cursor cursor) => Prepared.Running(cursor, 0);\n\n    {laws}\n}}\n"
    )
}

/// A law follows a nullable field a move copies, through `is int` and `?:`, after a move that takes a `Map` and after
/// one that takes none, as it follows a field that takes no `null`.
#[test]
fn a_law_follows_a_nullable_field_a_move_copies_through_is_int() {
    let laws = "law preparingKeepsTheStepMarksAndPosition(Cursor cursor, int position, Map<string, int> arguments) when position >= 0 => (Prepared.Running(cursor, position).prepare(arguments).cursor.step is int at ? (cursor.step is int was ? at == was : false) : cursor.step is null) && Prepared.Running(cursor, position).prepare(arguments).cursor.marks.count() == cursor.marks.count() && Prepared.Running(cursor, position).prepare(arguments).position == position;\n    law keepingKeepsTheStep(Cursor cursor, int position) => Prepared.Running(cursor, position).keep().cursor.step is int at ? (cursor.step is int was ? at == was : false) : cursor.step is null;";
    let directory = library_project(&[
        ("app/Runs/Mark.sharp", MARK),
        ("app/Runs/Cursor.sharp", CURSOR),
        ("app/Runs/Prepared.sharp", &prepared(laws)),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Runs/Prepared.sharp").exists());
}

/// A law that claims a move's copy of a nullable field grows by one is not proven, and the field's test is still to
/// show.
#[test]
fn a_false_law_over_a_nullable_field_a_move_copies_is_not_proven() {
    let law = "law preparingMovesTheStep(Cursor cursor, int position, Map<string, int> arguments) => Prepared.Running(cursor, position).prepare(arguments).cursor.step is int at ? (cursor.step is int was ? at == was + 1 : false) : cursor.step is null;";
    let directory = library_project(&[
        ("app/Runs/Mark.sharp", MARK),
        ("app/Runs/Cursor.sharp", CURSOR),
        ("app/Runs/Prepared.sharp", &prepared(law)),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains("error[law-unproven]: law preparingMovesTheStep is not proven\n = still to show: Prepared.Running(cursor, position).prepare(arguments).cursor.step is int at ? (cursor.step is int was ? at == was + 1 : false) : cursor.step is null\n"),
        "{}",
        printed(&output)
    );
    assert!(!compiled(root, "app/Runs/Prepared.sharp").exists());
}

/// `App.Runs.Phase`, an enum of three cases, two of which carry data.
const PHASE: &str = "namespace App.Runs;\n\npublic enum Phase\n{\n    case Idle;\n    case Busy(int job);\n    case Failed(string reason);\n}\n";

/// `App.Runs.Slot`, a struct that holds a `Phase` beside an int.
const SLOT: &str = "namespace App.Runs;\n\npublic struct Slot\n{\n    public Slot(public Phase phase { get; }, public int count { get; }) { }\n}\n";

/// `App.Runs.Worker`, whose `Running` case holds a `Slot`, with `advance`, which builds a new slot from its phase and
/// count, `keep`, which copies the slot, and `laws`.
fn worker(laws: &str) -> String {
    format!(
        "namespace App.Runs;\n\npublic machine Worker\n{{\n    case Running(Slot slot, int position)\n    {{\n        public Worker.Running advance() => Worker.Running(new Slot(this.slot.phase, this.slot.count), this.position + 1);\n\n        public Worker.Running keep() => Worker.Running(this.slot, this.position);\n    }}\n\n    public static Worker.Running start(Slot slot) => Worker.Running(slot, 0);\n\n    {laws}\n}}\n"
    )
}

/// A law follows an enum's case a move copies, through a test that names one case of three, as it follows each other
/// case where the test fails, after a move that builds a new struct around the case and after one that copies it. A
/// case that carries no data reads as a value pattern beside the cases that do.
#[test]
fn a_law_follows_an_enum_s_case_a_move_copies_through_a_test_that_names_one_case() {
    let laws = "law advancingKeepsThePhase(Slot slot, int position) when position >= 0 => (Worker.Running(slot, position).advance().slot.phase is Phase.Busy(int at) ? (slot.phase is Phase.Busy(int was) ? at == was : false) : slot.phase is Phase.Idle || (slot.phase is Phase.Failed(string reason) ? true : false)) && Worker.Running(slot, position).advance().slot.count == slot.count && Worker.Running(slot, position).advance().position == position + 1;\n    law keepingKeepsThePhase(Slot slot, int position) => Worker.Running(slot, position).keep().slot.phase is Phase.Busy(int at) ? (slot.phase is Phase.Busy(int was) ? at == was : false) : slot.phase is Phase.Idle || (slot.phase is Phase.Failed(string reason) ? true : false);\n    law advancingKeepsIdle(Slot slot, int position) => (Worker.Running(slot, position).advance().slot.phase is Phase.Idle) == (slot.phase is Phase.Idle);";
    let directory = library_project(&[
        ("app/Runs/Phase.sharp", PHASE),
        ("app/Runs/Slot.sharp", SLOT),
        ("app/Runs/Worker.sharp", &worker(laws)),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Runs/Worker.sharp").exists());
}

/// A law that claims a move's copy of an enum's case changes the case's data is not proven, and the case's test is
/// still to show.
#[test]
fn a_false_law_over_an_enum_s_case_a_move_copies_is_not_proven() {
    let law = "law advancingMovesTheJob(Slot slot, int position) => Worker.Running(slot, position).advance().slot.phase is Phase.Busy(int at) ? (slot.phase is Phase.Busy(int was) ? at == was + 1 : false) : slot.phase is Phase.Idle || (slot.phase is Phase.Failed(string reason) ? true : false);";
    let directory = library_project(&[
        ("app/Runs/Phase.sharp", PHASE),
        ("app/Runs/Slot.sharp", SLOT),
        ("app/Runs/Worker.sharp", &worker(law)),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains("error[law-unproven]: law advancingMovesTheJob is not proven\n = still to show: Worker.Running(slot, position).advance().slot.phase is Phase.Busy(int at) ? (slot.phase is Phase.Busy(int was) ? at == was + 1 : false) : slot.phase is Phase.Idle || (slot.phase is Phase.Failed(string reason) ? true : false)\n"),
        "{}",
        printed(&output)
    );
    assert!(!compiled(root, "app/Runs/Worker.sharp").exists());
}

/// A `List` is Lean's `List`, and its `map`, `filter` and `count` are Lean's `List.map`, `List.filter` and
/// `List.length`, so Mago proves a law over them with the proof it creates.
#[test]
fn compile_proves_a_law_over_map_filter_and_count() {
    assert_proven(
        "Basket",
        "namespace App.Shared;\n\npublic class Basket\n{\n    public static List<bool> flags(List<int> items) => items.map(x => x > 0);\n\n    public static List<int> positives(List<int> items) => items.filter(x => x > 0);\n\n    law flagsKeepCount(List<int> items) => Basket.flags(items).count() == items.count();\n\n    law positivesDropTheRest(int unused) => Basket.positives([1, -2, 3]) == [1, 3];\n}\n",
        "law positivesKeepAll(List<int> items) => Basket.positives(items).count() == items.count();",
    );
}

/// Spec section 28: a law reads a List's `all` and `any` of a lambda, and a Map's `any` of one over its values, as it
/// reads their `filter`, so a rule that every value or some value matches proves as it is written.
#[test]
fn a_law_reads_a_lists_all_and_any_and_a_maps_any() {
    let directory = library_project(&[(
        "app/Probe/Scores.sharp",
        "namespace App.Probe;\n\ninternal final class Scores\n{\n    private static bool allPositive(List<int> values) => values.all((int value) => value > 0);\n\n    private static bool noneAtMostZero(List<int> values) => values.any((int value) => value <= 0) == false;\n\n    private static bool anyZero(Map<string, int> scores) => scores.any((int score) => score == 0);\n\n    law aNegativeValueFailsAll(int negative) => negative >= 0 || !Scores.allPositive([1, negative]);\n\n    law aNegativeValueFailsAny(int negative) => negative >= 0 || !Scores.noneAtMostZero([1, negative]);\n\n    law aZeroScoreIsFound(string name, int score) => !Scores.anyZero([name: score]) || score == 0;\n}\n",
    )]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Probe/Scores.sharp").exists());
}

/// `App.Jobs.Retry`, an enum whose `backoff()` gives each case's growing list of delays, of 0 to 9 entries,
/// with `law`.
fn retry(law: &str) -> String {
    format!(
        "namespace App.Jobs;\n\npublic enum Retry : string\n{{\n    case None = \"none\";\n    case Fixed = \"fixed\";\n    case Linear = \"linear\";\n    case Patient = \"patient\";\n    case Exponential = \"exponential\";\n    case ExponentialRamp = \"exponential-ramp\";\n\n    public List<int> backoff() => match (this) {{\n        Retry.None => [],\n        Retry.Fixed => [5, 5, 5],\n        Retry.Linear => [10, 20, 30],\n        Retry.Patient => [1, 2, 3, 5, 8, 13, 21, 34, 55],\n        Retry.Exponential => [1, 2, 4, 8, 16, 32, 64],\n        Retry.ExponentialRamp => [30, 60, 300, 900, 1800, 3600, 7200],\n    }};\n\n    {law}\n}}\n"
    )
}

/// A `List` read `x[i]` is the element at `i`, and throws `OutOfRangeException` at an index outside the list, as the
/// engine does, and a throw does not break a law. So a law proves through its own guards, and a law without them proves
/// where the read is in range, at an index the law leaves open, over every list an enum's method returns.
#[test]
fn compile_proves_a_law_over_a_list_index_read() {
    let laws = "law backoffNeverShrinks(Retry retry, int index) => index < 0 || index + 1 >= retry.backoff().count() || retry.backoff()[index] <= retry.backoff()[index + 1];\n    law rampNeverShrinks(int index) => index < 0 || index + 1 >= Retry.ExponentialRamp.backoff().count() || Retry.ExponentialRamp.backoff()[index] <= Retry.ExponentialRamp.backoff()[index + 1];\n    law rampStartsAtThirty(int unused) => Retry.ExponentialRamp.backoff()[0] == 30;\n    law backoffWaits(Retry retry, int index) => retry.backoff()[index] >= 1;";
    let directory = library_project(&[("app/Jobs/Retry.sharp", &retry(laws))]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Jobs/Retry.sharp").exists());
}

/// A law an index read breaks, as a ramp of delays that shrinks from 30 to 20, is not proven, and the goal still to show
/// names the reads.
#[test]
fn a_false_law_over_a_list_index_read_is_not_proven() {
    let shrinking = retry(
        "law rampNeverShrinks(int index) => index < 0 || index + 1 >= Retry.ExponentialRamp.backoff().count() || Retry.ExponentialRamp.backoff()[index] <= Retry.ExponentialRamp.backoff()[index + 1];",
    )
    .replace("[30, 60, 300", "[30, 20, 300");
    let directory = library_project(&[("app/Jobs/Retry.sharp", &shrinking)]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "error[law-unproven]: law rampNeverShrinks is not proven\n = still to show: index < 0 || index + 1 >= Retry.ExponentialRamp.backoff().count() || Retry.ExponentialRamp.backoff()[index] <= Retry.ExponentialRamp.backoff()[index + 1]\n"
        ),
        "{}",
        printed(&output)
    );
}

/// A law without a body over an enum it takes that proves case by case reports each case it does not prove on its own,
/// with the case it is: over a ramp of delays that shrinks from 30 to 20, only `Retry.ExponentialRamp`, and with
/// `Retry.Linear`'s delays shrinking from 20 to 15 too, both.
#[test]
fn a_law_without_a_body_reports_each_enum_case_it_does_not_prove() {
    let claim =
        "index < 0 || index + 1 >= retry.backoff().count() || retry.backoff()[index] <= retry.backoff()[index + 1]";
    let ramp = retry(&format!("law backoffNeverShrinks(Retry retry, int index) => {claim};"))
        .replace("[30, 60, 300", "[30, 20, 300");
    let linear = ramp.replace("[10, 20, 30]", "[10, 20, 15]");
    for (source, cases) in
        [(ramp, vec!["Retry.ExponentialRamp"]), (linear, vec!["Retry.Linear", "Retry.ExponentialRamp"])]
    {
        let directory = library_project(&[("app/Jobs/Retry.sharp", &source)]);

        let output = compile(directory.path());

        assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
        for case in &cases {
            assert!(
                printed(&output).contains(&format!(
                    "error[law-unproven]: law backoffNeverShrinks is not proven\n = when retry is {case}, still to show: {claim}\n"
                )),
                "{}",
                printed(&output)
            );
        }
        assert_eq!(
            printed(&output).matches("law backoffNeverShrinks is not proven").count(),
            cases.len(),
            "{}",
            printed(&output)
        );
    }
}

/// A case whose proof runs out of its time budget is not proven, as any case left open is: it is refused as
/// `law-unproven`, names the case, and says the proof ran out of time. The case after it still runs, and is reported on
/// its own. The slow case's claim has 18 `||` choices for `omega` to split on, 2^18 cases in all.
#[test]
fn a_law_whose_proof_runs_out_of_time_is_not_proven_and_its_other_cases_still_run() {
    let names = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r"];
    let bits = names.map(|name| format!("({name} == 0 || {name} == 1)")).join(" && ");
    let claim = format!("slow ? !({bits}) || {} <= 18 : a < a", names.join(" + "));
    let parameters = names.map(|name| format!("int {name}")).join(", ");
    let source = format!(
        "namespace App.Shared;\n\npublic class Steps\n{{\n    law bits(bool slow, {parameters})\n        => {claim}\n    {{\n        if (slow) {{\n            return;\n        }}\n        return;\n    }}\n}}\n"
    );
    let directory = lawful_project(&[("app/Shared/Steps.sharp", &source)]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    for refused in [
        format!(
            "error[law-unproven]: law bits is not proven\n = when slow, still to show: {claim}\n = the proof ran out of time before it finished\n"
        ),
        format!(
            "error[law-unproven]: law bits is not proven\n = when !slow, still to show: a < a\n = fails for: slow = false, {}\nerror",
            names.map(|name| format!("{name} = 0")).join(", ")
        ),
    ] {
        assert!(printed(&output).contains(&refused), "{}", printed(&output));
    }
    assert!(!printed(&output).contains("bug in the PHP# checker"), "{}", printed(&output));
}

/// A law body's `match` with an arm for every case of an enum is exhaustive, as the same `match` in a method is, and
/// splits the law's proof on the cases.
#[test]
fn compile_proves_a_law_whose_body_matches_every_case_of_an_enum() {
    assert_backoff_split_on("Retry.ExponentialRamp", "retry is Retry.ExponentialRamp");
}

/// Checks that `backoffNeverShrinks`, with a body that matches `retry` on each case and then `last`, proves, and that
/// over a ramp of delays that shrinks from 30 to 20 it is refused in the arm `when` names.
fn assert_backoff_split_on(last: &str, when: &str) {
    let claim =
        "index < 0 || index + 1 >= retry.backoff().count() || retry.backoff()[index] <= retry.backoff()[index + 1]";
    let law = format!(
        "law backoffNeverShrinks(Retry retry, int index) => {claim}\n    {{\n        match (retry) {{\n            Retry.None => {{ return; }},\n            Retry.Fixed => {{ return; }},\n            Retry.Linear => {{ return; }},\n            Retry.Patient => {{ return; }},\n            Retry.Exponential => {{ return; }},\n            {last} => {{ return; }},\n        }}\n    }}"
    );
    for (ramp, proven) in [("[30, 60, 300", true), ("[30, 20, 300", false)] {
        let source = retry(&law).replace("[30, 60, 300", ramp);
        let directory = library_project(&[("app/Jobs/Retry.sharp", &source)]);
        let root = directory.path();

        let output = compile(root);

        assert_eq!(output.status.success(), proven, "{ramp}: {}", printed(&output));
        assert_eq!(compiled(root, "app/Jobs/Retry.sharp").exists(), proven);
        if !proven {
            assert!(
                printed(&output).contains(&format!(
                    "error[law-unproven]: law backoffNeverShrinks is not proven\n = when {when}, still to show: {claim}\n"
                )),
                "{}",
                printed(&output)
            );
        }
    }
}

/// A law body's `match` arm that joins an enum's cases with `or` is a case for each case it joins, as a `match` with an
/// arm for each case is.
#[test]
fn compile_proves_a_law_whose_body_matches_cases_joined_with_or() {
    let directory = library_project(&[("app/Jobs/Retry.sharp", &joined_retry("[30, 60, 300]"))]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Jobs/Retry.sharp").exists());
}

/// A law whose `or` arm is false for one of the cases it joins, as over a ramp of delays that shrinks from 30 to 20, is
/// not proven in that case, and the error names that case alone.
#[test]
fn a_law_whose_body_joins_cases_with_or_is_refused_in_the_case_it_does_not_prove() {
    let directory = library_project(&[("app/Jobs/Retry.sharp", &joined_retry("[30, 20, 300]"))]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(&format!(
            "error[law-unproven]: law backoffNeverShrinks is not proven\n = when retry is Retry.ExponentialRamp, still to show: {JOINED_CLAIM}\n"
        )),
        "{}",
        printed(&output)
    );
    assert_eq!(printed(&output).matches("error[law-unproven]").count(), 1, "{}", printed(&output));
}

/// The claim of `joined_retry`'s law.
const JOINED_CLAIM: &str =
    "index < 0 || index + 1 >= retry.backoff().count() || retry.backoff()[index] <= retry.backoff()[index + 1]";

/// `App.Jobs.Retry`, an enum of three cases whose `ExponentialRamp` delays are `ramp`, with the law that its delays
/// never shrink, proven by a `match` whose second arm joins `Retry.Exponential` and `Retry.ExponentialRamp` with `or`.
fn joined_retry(ramp: &str) -> String {
    format!(
        "namespace App.Jobs;\n\npublic enum Retry : string\n{{\n    case None = \"none\";\n    case Exponential = \"exponential\";\n    case ExponentialRamp = \"exponential-ramp\";\n\n    public List<int> backoff() => match (this) {{\n        Retry.None => [],\n        Retry.Exponential => [1, 2, 4, 8],\n        Retry.ExponentialRamp => {ramp},\n    }};\n\n    law backoffNeverShrinks(Retry retry, int index) => {JOINED_CLAIM}\n    {{\n        match (retry) {{\n            Retry.None => {{ return; }},\n            Retry.Exponential or Retry.ExponentialRamp => {{ return; }},\n        }}\n    }}\n}}\n"
    )
}

/// Compiles `source` as `app/Shared/<class>.sharp`, in a project that installs the standard library and so the members
/// of a `List`, and checks that the compile proves every law it states with the proof
/// it creates, and that the class with `twin` added, a false law over the same code, is refused as `law-unproven`.
fn assert_proven(class: &str, source: &str, twin: &str) {
    let name = format!("app/Shared/{class}.sharp");
    let with_twin = with_twin(source, twin);
    let Some(law) = twin.strip_prefix("law ").and_then(|twin| twin.split('(').next()) else {
        panic!("{twin} is no law");
    };

    for (source, proven) in [(source, true), (with_twin.as_str(), false)] {
        let directory = library_project(&[(&name, source)]);
        let root = directory.path();

        let output = compile(root);

        assert_eq!(output.status.success(), proven, "{source}: {}", printed(&output));
        assert_eq!(compiled(root, &name).exists(), proven);
        if !proven {
            assert!(
                printed(&output).contains(&format!("error[law-unproven]: law {law} is not proven")),
                "{}",
                printed(&output)
            );
        }
    }
}

/// `source` with `twin`, a law, stated last in its class.
fn with_twin(source: &str, twin: &str) -> String {
    let Some(end) = source.rfind('}') else {
        panic!("{source} has no closing brace");
    };

    format!("{}    {twin}\n{}", &source[..end], &source[end..])
}

/// Spec section 28.1's `total`: a method that calls itself on a list's rest from a list pattern gets smaller on each call.
#[test]
fn compile_proves_a_law_over_recursion_on_a_list_rest() {
    assert_proven(
        "Money",
        "namespace App.Shared;\n\npublic class Money\n{\n    public Money(public int amount { get; }, public string currency { get; }) { }\n\n    public static int total(List<Money> items) => match (items) {\n        [] => 0,\n        [Money first, ...List<Money> rest] => first.amount + Money.total(rest),\n    };\n\n    law totalOfTwo(int unused) => Money.total([new Money(1, \"EUR\"), new Money(2, \"EUR\")]) == 3;\n}\n",
        "law totalOfTwoIsFour(int unused) => Money.total([new Money(1, \"EUR\"), new Money(2, \"EUR\")]) == 4;",
    );
}

/// Spec section 28.1's `times`: `n - 1` where `n <= 0` failed is one step towards the bound 0.
#[test]
fn compile_proves_a_law_over_recursion_on_an_int_one_step_towards_a_bound_a_guard_tested() {
    assert_proven(
        "Money",
        "namespace App.Shared;\n\npublic class Money\n{\n    public Money(public int amount { get; }, public string currency { get; }) { }\n    public Money add(Money other) => new Money(this.amount + other.amount, this.currency);\n    public Money times(int n) => n <= 0 ? new Money(0, this.currency) : this.add(this.times(n - 1));\n\n    law zeroTimesIsZero(Money m) => m.times(0).amount == 0;\n}\n",
        "law oneTimeIsZero(Money m) => m.times(1).amount == 0;",
    );
}

/// An `if` that returns, a `match` arm's comparison, and a `?:` each show the bound a step moves towards, a literal or a
/// parameter the call passes on unchanged.
#[test]
fn compile_proves_a_law_over_recursion_towards_a_bound_an_if_a_match_or_a_parameter_sets() {
    assert_proven(
        "Steps",
        "namespace App.Shared;\n\npublic class Steps\n{\n    public static int walk(int n)\n    {\n        if (n <= 0) {\n            return 0;\n        }\n        return 1 + Steps.walk(n - 1);\n    }\n\n    public static int fall(int n) => match (n) {\n        <= 0 => 0,\n        default => Steps.fall(n - 1),\n    };\n\n    public static int climb(int i, int top) => i >= top ? 0 : 1 + Steps.climb(i + 1, top);\n\n    law twoSteps(int unused) => Steps.walk(2) == 2;\n    law fallsToZero(int unused) => Steps.fall(2) == 0;\n    law climbsTwo(int unused) => Steps.climb(0, 2) == 2;\n}\n",
        "law climbsThree(int unused) => Steps.climb(0, 2) == 3;",
    );
}

/// Two methods of one class that call each other on `n - 1` under the same bound are one Lean `mutual` block.
#[test]
fn compile_proves_a_law_over_mutual_recursion() {
    assert_proven(
        "Parity",
        "namespace App.Shared;\n\npublic class Parity\n{\n    public static bool isEven(int n) => n <= 0 ? true : Parity.isOdd(n - 1);\n    public static bool isOdd(int n) => n <= 0 ? false : Parity.isEven(n - 1);\n\n    law fourIsEven(int unused) => Parity.isEven(4);\n}\n",
        "law threeIsEven(int unused) => Parity.isEven(3);",
    );
}

/// A list pattern names an element or only its type, and has a rest anywhere: before the last element, or bare.
#[test]
fn compile_proves_a_law_over_list_patterns_with_a_binding_arm_and_a_type_only_arm() {
    assert_proven(
        "Basket",
        "namespace App.Shared;\n\npublic class Basket\n{\n    public Basket(public int amount { get; }) { }\n\n    public static int size(List<Basket> items) => match (items) {\n        [] => 0,\n        [Basket, ...List<Basket> rest] => 1 + Basket.size(rest),\n    };\n\n    public static int firstAmount(List<Basket> items) => match (items) {\n        [Basket first, ...] => first.amount,\n        default => 0,\n    };\n\n    public static int lastAmount(List<Basket> items) => match (items) {\n        [..., Basket last] => last.amount,\n        default => 0,\n    };\n\n    public static int backwards(List<Basket> items) => match (items) {\n        [...List<Basket> before, Basket last] => last.amount + Basket.backwards(before),\n        default => 0,\n    };\n\n    law sizeOfTwo(int unused) => Basket.size([new Basket(1), new Basket(2)]) == 2;\n    law firstOfTwo(int unused) => Basket.firstAmount([new Basket(1), new Basket(2)]) == 1;\n    law lastOfTwo(int unused) => Basket.lastAmount([new Basket(1), new Basket(2)]) == 2;\n    law backwardsOfTwo(int unused) => Basket.backwards([new Basket(1), new Basket(2)]) == 3;\n}\n",
        "law lastOfTwoIsFirst(int unused) => Basket.lastAmount([new Basket(1), new Basket(2)]) == 1;",
    );
}

#[test]
fn compile_proves_a_law_over_a_match_statement_with_block_arms_and_a_when_guard() {
    assert_proven(
        "Sign",
        "namespace App.Shared;\n\npublic class Sign\n{\n    public static int of(int n)\n    {\n        match (n) {\n            int m when m > 0 => { return 1; },\n            0 => { return 0; },\n            default => { return -1; },\n        }\n    }\n\n    law positive(int unused) => Sign.of(5) == 1;\n    law zero(int unused) => Sign.of(0) == 0;\n}\n",
        "law negativeIsZero(int unused) => Sign.of(-5) == 0;",
    );
}

/// `is` tests the type a value of a nullable type holds when it is not null, and names it in the `?:` it decides.
#[test]
fn compile_proves_a_law_over_an_is_test() {
    assert_proven(
        "Order",
        "namespace App.Shared;\n\npublic class Order\n{\n    public Order(public int total { get; }) { }\n\n    public static bool known(Order? order) => order is Order;\n    public static int totalOf(Order? order) => order is Order o ? o.total : 0;\n\n    law nullIsUnknown(int unused) => !Order.known(null);\n    law nullTotalIsZero(int unused) => Order.totalOf(null) == 0;\n    law totalIsKept(Order order) => Order.totalOf(order) == order.total;\n}\n",
        "law totalIsZero(Order order) => Order.totalOf(order) == 0;",
    );
}

#[test]
fn compile_proves_a_law_over_a_spread() {
    assert_proven(
        "Joiner",
        "namespace App.Shared;\n\npublic class Joiner\n{\n    public static List<int> joined(List<int> a, List<int> b) => [...a, 0, ...b];\n\n    law joinedKeepsCount(List<int> a, List<int> b) => Joiner.joined(a, b).count() == a.count() + b.count() + 1;\n}\n",
        "law joinedAddsNothing(List<int> a, List<int> b) => Joiner.joined(a, b).count() == a.count() + b.count();",
    );
}

/// `count()` is a value, so a lambda, a `when` guard and a `?:` condition read it.
#[test]
fn compile_proves_a_law_over_a_count_in_a_lambda_a_guard_and_a_condition() {
    assert_proven(
        "Lists",
        "namespace App.Shared;\n\npublic class Lists\n{\n    public static List<List<int>> nonEmpty(List<List<int>> lists) => lists.filter(list => list.count() > 0);\n\n    public static int size(List<int> xs) => match (xs) {\n        [int first, ...] when xs.count() > 2 => first,\n        default => xs.count() > 1 ? 1 : 0,\n    };\n\n    law dropsEmpty(int unused) => Lists.nonEmpty([[1], [2, 3]]).count() == 2;\n    law longFirst(int unused) => Lists.size([5, 6, 7]) == 5;\n    law shortIsOne(int unused) => Lists.size([5, 6]) == 1;\n}\n",
        "law keepsEmpty(int unused) => Lists.nonEmpty([[1], []]).count() == 2;",
    );
}

/// `[]` is the empty List wherever the code expects a List: an argument, a returned value, and a compared one.
#[test]
fn compile_proves_a_law_over_an_empty_list() {
    assert_proven(
        "More",
        "namespace App.Shared;\n\npublic class More\n{\n    public static int sizeOf(List<int> xs) => match (xs) {\n        [] => 0,\n        default => xs.count() > 2 ? 2 : 1,\n    };\n\n    public static List<int> none(int unused) => [];\n\n    law sizeOfEmpty(int unused) => More.sizeOf([]) == 0;\n    law noneIsEmpty(int unused) => More.none(0).count() == 0;\n    law noneIsTheEmptyList(int unused) => More.none(0) == [];\n}\n",
        "law sizeOfEmptyIsOne(int unused) => More.sizeOf([]) == 1;",
    );
}

/// A loop that only counts is the list's length.
#[test]
fn compile_proves_a_law_over_a_counting_loop() {
    assert_proven(
        "Orders",
        "namespace App.Shared;\n\npublic class Orders\n{\n    public static int size(List<int> orders)\n    {\n        let count = 0;\n        for (const order of orders) {\n            count++;\n        }\n        return count;\n    }\n\n    law sizeIsCount(List<int> orders) => Orders.size(orders) == orders.count();\n}\n",
        "law sizeIsOneMore(List<int> orders) => Orders.size(orders) == orders.count() + 1;",
    );
}

/// A loop that adds each element is `List.sum`, so Lean's `List.sum_append` proves that it joins.
#[test]
fn compile_proves_a_law_over_a_summing_loop() {
    assert_proven(
        "Sums",
        "namespace App.Shared;\n\npublic class Sums\n{\n    public static int sumOf(List<int> xs)\n    {\n        let sum = 0;\n        for (const x of xs) {\n            sum += x;\n        }\n        return sum;\n    }\n\n    law sumJoins(List<int> a, List<int> b) => Sums.sumOf([...a, ...b]) == Sums.sumOf(a) + Sums.sumOf(b);\n}\n",
        "law sumIsTheFirst(List<int> a, List<int> b) => Sums.sumOf([...a, ...b]) == Sums.sumOf(a);",
    );
}

/// A recursive sum over a list's rest, and a law its body proves by calling itself on that rest, as the laws design
/// writes them: each `match` handles every length of the list, so neither has `default`.
const SUMMED_MONEY: &str = "namespace App.Shared;\n\npublic class Money\n{\n    public Money(public int amount { get; }, public string currency { get; }) { }\n\n    public static int total(List<Money> items) => match (items) {\n        [] => 0,\n        [Money first, ...List<Money> rest] => first.amount + Money.total(rest),\n    };\n\n    law totalJoins(List<Money> a, List<Money> b) => Money.total([...a, ...b]) == Money.total(a) + Money.total(b)\n    {\n        match (a) {\n            [] => { return; },\n            [Money, ...List<Money> rest] => { Money.totalJoins(rest, b); },\n        }\n    }\n}\n";

const TOTAL_JOINS_FIRST: &str = "law totalJoinsFirst(List<Money> a, List<Money> b) => Money.total([...a, ...b]) == Money.total(a)\n    {\n        match (a) {\n            [] => { return; },\n            [Money, ...List<Money> rest] => { Money.totalJoinsFirst(rest, b); },\n        }\n    }";

/// Three recursive sums over a list, one of two fields of each element, and a law its body proves by calling itself on
/// the list's rest.
const LINES: &str = "namespace App.Shared;\n\npublic class Line\n{\n    public Line(public int net { get; }, public int tax { get; }) { }\n\n    public static int nets(List<Line> lines) => match (lines) {\n        [] => 0,\n        [Line first, ...List<Line> rest] => first.net + Line.nets(rest),\n        default => 0,\n    };\n\n    public static int taxes(List<Line> lines) => match (lines) {\n        [] => 0,\n        [Line first, ...List<Line> rest] => first.tax + Line.taxes(rest),\n        default => 0,\n    };\n\n    public static int gross(List<Line> lines) => match (lines) {\n        [] => 0,\n        [Line first, ...List<Line> rest] => first.net + first.tax + Line.gross(rest),\n        default => 0,\n    };\n\n    law grossIsNetsAndTaxes(List<Line> lines) => Line.gross(lines) == Line.nets(lines) + Line.taxes(lines)\n    {\n        match (lines) {\n            [] => { return; },\n            [Line, ...List<Line> rest] => { Line.grossIsNetsAndTaxes(rest); },\n            default => { return; },\n        }\n    }\n}\n";

const GROSS_IS_NETS: &str = "law grossIsNets(List<Line> lines) => Line.gross(lines) == Line.nets(lines)\n    {\n        match (lines) {\n            [] => { return; },\n            [Line, ...List<Line> rest] => { Line.grossIsNets(rest); },\n            default => { return; },\n        }\n    }";

/// The law for the list's rest holds wherever `total` runs, since a claim computes ints exactly.
#[test]
fn compile_proves_by_recursion_that_the_sum_of_a_joined_list_is_the_sum_of_its_parts() {
    assert_proven("Money", SUMMED_MONEY, TOTAL_JOINS_FIRST);
}

/// A claim computes ints exactly, so a claim true only because its own `+` throws at `PHP_INT_MAX` is not proven.
#[test]
fn a_claim_true_only_because_its_own_arithmetic_overflows_is_not_proven() {
    let directory = lawful_project(&[(
        "app/Shared/Bound.sharp",
        "namespace App.Shared;\n\npublic class Bound\n{\n    law fits(int x) => x + 1 <= 9223372036854775807;\n}\n",
    )]);
    let root = directory.path();

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "app/Shared/Bound.sharp:5:5: error[law-unproven]: law fits is not proven\n = still to show: x + 1 <= 9223372036854775807\n"
        ),
        "{}",
        printed(&output)
    );
}

/// A law whose goal stays open runs on small values, and a false law prints the first values that break it, each
/// parameter as PHP# writes its value.
#[test]
fn a_false_law_prints_the_small_values_that_break_it() {
    let directory = lawful_project(&[(
        "app/Shared/Account.sharp",
        "namespace App.Shared;\n\npublic final class Account\n{\n    public Account(public int balance { get; }) { }\n\n    public Account deposit(int amount) => new Account(this.balance + amount);\n\n    law depositNeverShrinks(Account a, int amount) => a.deposit(amount).balance >= a.balance;\n}\n",
    )]);
    let root = directory.path();

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains("error[law-unproven]: law depositNeverShrinks is not proven\n"),
        "{}",
        printed(&output)
    );
    assert!(printed(&output).contains(" = fails for: a = new Account(0), amount = -1\n"), "{}", printed(&output));
}

/// A law no small value breaks is still not proven, and says it may be true and needs a proof body.
#[test]
fn a_law_no_small_value_breaks_is_refused_as_needing_a_proof_body() {
    let directory = lawful_project(&[(
        "app/Shared/Money.sharp",
        &with_twin(
            SUMMED_MONEY,
            "law totalJoinsAgain(List<Money> a, List<Money> b) => Money.total([...a, ...b]) == Money.total(a) + Money.total(b);",
        ),
    )]);
    let root = directory.path();

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains("error[law-unproven]: law totalJoinsAgain is not proven\n"),
        "{}",
        printed(&output)
    );
    assert!(
        printed(&output).contains(" = no small value breaks it, so it may be true: write a proof body that shows it\n"),
        "{}",
        printed(&output)
    );
    assert!(!printed(&output).contains("fails for"), "{}", printed(&output));
}

/// `App.Books.Ledger`, the specimen of spec section 28's interface laws: an interface whose law says what `credit`
/// gives.
const LEDGER: &str = "namespace App.Books;\n\npublic interface Ledger\n{\n    int balance { get; }\n    Ledger credit(int amount);\n\n    law creditAdds(Ledger l, int amount) when amount >= 0 => l.credit(amount).balance == l.balance + amount;\n}\n";

/// `App.Books.MemoryLedger`, a PHP# implementer of [`LEDGER`] whose `credit` adds `amount` and `extra`.
fn memory_ledger(extra: &str) -> String {
    format!(
        "namespace App.Books;\n\npublic class MemoryLedger : Ledger\n{{\n    public MemoryLedger(public int balance {{ get; }}) {{ }}\n    public Ledger credit(int amount) => new MemoryLedger(this.balance + amount{extra});\n}}\n"
    )
}

/// Spec sections 22 and 28: each PHP# implementer of an interface proves each of its laws, with the interface's values
/// read as its own, so one that breaks a law is refused, naming its method that the law calls and the law.
#[test]
fn an_implementer_that_breaks_its_interface_s_law_is_refused_naming_the_law() {
    let directory =
        lawful_project(&[("app/Books/Ledger.sharp", LEDGER), ("app/Books/MemoryLedger.sharp", &memory_ledger(" + 1"))]);
    let root = directory.path();

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "app/Books/MemoryLedger.sharp:3:14: error[law-unproven]: `MemoryLedger.credit` breaks `Ledger.creditAdds`"
        ),
        "{}",
        printed(&output)
    );
    assert!(!compiled(root, "app/Books/MemoryLedger.sharp").exists());
}

/// An implementer that keeps its interface's law compiles.
#[test]
fn an_implementer_that_keeps_its_interface_s_law_compiles() {
    let directory =
        lawful_project(&[("app/Books/Ledger.sharp", LEDGER), ("app/Books/MemoryLedger.sharp", &memory_ledger(""))]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Books/MemoryLedger.sharp").exists());
    assert!(compiled(root, "app/Books/Ledger.sharp").exists());
}

/// A plain PHP class can't prove an interface's laws, so it implements no interface that states one.
#[test]
fn a_plain_php_class_implementing_an_interface_with_laws_is_refused() {
    let directory = lawful_project(&[
        ("app/Books/Ledger.sharp", LEDGER),
        (
            "app/Books/FileLedger.php",
            "<?php\n\nnamespace App\\Books;\n\nfinal class FileLedger implements Ledger\n{\n    public int $balance = 0;\n\n    public function credit(int $amount): Ledger\n    {\n        return $this;\n    }\n}\n",
        ),
    ]);
    let root = directory.path();

    let output = compile_lines(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "error - interface-law-implementer: `App.Books.FileLedger` is plain PHP, which can't prove `Ledger.creditAdds`. Write it in PHP#."
        ),
        "{}",
        printed(&output)
    );
}

/// Spec section 28: a law over an interface's values uses the interface's laws as facts, which each implementer
/// proves, by calling them in its body.
#[test]
fn a_law_over_an_interface_s_values_uses_the_interface_s_laws_as_facts() {
    let directory = lawful_project(&[
        ("app/Books/Ledger.sharp", LEDGER),
        ("app/Books/MemoryLedger.sharp", &memory_ledger("")),
        (
            "app/Books/Ledgers.sharp",
            "namespace App.Books;\n\npublic final class Ledgers\n{\n    law creditingNothingKeepsTheBalance(Ledger l) => l.credit(0).balance == l.balance\n    {\n        Ledger.creditAdds(l, 0);\n    }\n}\n",
        ),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Books/Ledgers.sharp").exists());
}

/// A nullable int is a value or null, so a claim compares it as code does, with no exact `Int` to read.
#[test]
fn compile_proves_a_law_that_compares_nullable_ints() {
    assert_proven(
        "Box",
        "namespace App.Shared;\n\npublic class Box\n{\n    public static int? keep(int? x) => x;\n\n    law keepKeeps(int? x) => Box.keep(x) == x;\n}\n",
        "law keepIsNull(int? x) => Box.keep(x) == null;",
    );
}

/// A sum of two fields of each element is the sum of their two sums, which the law for the list's rest proves.
#[test]
fn compile_proves_by_recursion_that_a_sum_of_two_fields_is_the_sum_of_their_sums() {
    assert_proven("Line", LINES, GROSS_IS_NETS);
}

/// A case a sum law leaves open prints its goal in PHP#: the claim's exact ints as the code writes them, each value a
/// step binds where it is used, though two steps bind theirs under one name, and an element the arm leaves unnamed by
/// its place in the list, as `lines[0]`.
#[test]
fn a_case_a_sum_law_leaves_open_names_its_goal_in_php_sharp() {
    let directory = lawful_project(&[
        ("app/Shared/Money.sharp", &with_twin(SUMMED_MONEY, TOTAL_JOINS_FIRST)),
        ("app/Shared/Line.sharp", &with_twin(LINES, GROSS_IS_NETS)),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    for expected in [
        "app/Shared/Money.sharp:19:5: error[law-unproven]: law totalJoinsFirst is not proven\n = when a is [], still to show: Money.total([...a, ...b]) == Money.total(a)\n",
        "app/Shared/Line.sharp:33:5: error[law-unproven]: law grossIsNets is not proven\n = when lines is [Line, ...List<Line> rest], still to show: lines[0].net + lines[0].tax + Line.gross(rest) == lines[0].net + Line.nets(rest)\n",
    ] {
        assert!(printed(&output).contains(expected), "{}", printed(&output));
    }
}

/// A loop that counts the elements an `if` tests is `List.countP`, so Lean's `List.countP_le_length` proves it counts no
/// more than the list holds.
#[test]
fn compile_proves_a_law_over_a_counting_loop_under_an_if() {
    assert_proven(
        "Order",
        "namespace App.Shared;\n\npublic class Order\n{\n    public Order(public bool open { get; }) { }\n\n    public static int openCount(List<Order> orders)\n    {\n        let count = 0;\n        for (const o of orders) {\n            if (o.open) {\n                count += 1;\n            }\n        }\n        return count;\n    }\n\n    law countAtMost(List<Order> orders) => Order.openCount(orders) <= orders.count();\n}\n",
        "law countAll(List<Order> orders) => Order.openCount(orders) == orders.count();",
    );
}

/// Spec section 28's `Money`: a law the checker proves alone, a law its body proves by recursion on a smaller int, one
/// proven by recursion on a list's rest, and one under a `when` condition.
const SPECIMEN_MONEY: &str = "namespace App.Shared;\n\npublic class Money\n{\n    public Money(public int amount { get; }, public string currency { get; }) { }\n    public Money add(Money other) => new Money(this.amount + other.amount, this.currency);\n    public Money times(int n) => n <= 0 ? new Money(0, this.currency) : this.add(this.times(n - 1));\n\n    law addKeepsCurrency(Money a, Money b) => a.add(b).currency == a.currency;\n    law timesKeepsCurrency(Money m, int n) => m.times(n).currency == m.currency\n    {\n        if (n <= 0) {\n            return;\n        }\n        Money.timesKeepsCurrency(m, n - 1);\n        Money.addKeepsCurrency(m, m.times(n - 1));\n    }\n\n    public static Money sum(Money total, List<Money> items) => match (items) {\n        [] => total,\n        [Money first, ...List<Money> rest] => Money.sum(total.add(first), rest),\n    };\n\n    law sumKeepsCurrency(Money total, List<Money> items) => Money.sum(total, items).currency == total.currency\n    {\n        match (items) {\n            [] => { return; },\n            [Money first, ...List<Money> rest] => {\n                Money.sumKeepsCurrency(total.add(first), rest);\n                Money.addKeepsCurrency(total, first);\n            },\n        }\n    }\n\n    public static int refundable(int paid, int refunded) => Math.max(paid - refunded, 0);\n\n    law neverAbovePaid(int paid, int refunded) when paid >= 0 && refunded >= 0\n        => Money.refundable(paid, refunded) <= paid;\n}\n";

/// The standard library's `Math.max` and `Math.min` on ints are pure and reach laws with no `extern`: the checker
/// knows what they compute, with a spread argument as with one written out, and a claim they break is not proven.
#[test]
fn a_law_reasons_about_math_max_and_math_min_as_the_largest_and_the_smallest_int() {
    let bounds = |law: &str| {
        format!(
            "namespace App.Shared;\n\npublic class Bounds\n{{\n    public static int clamp(int x, int low, int high) => Math.min(Math.max(x, low), high);\n    public static int largest(int first, List<int> rest) => Math.max(first, ...rest);\n\n    {law}\n}}\n"
        )
    };
    for (law, proven) in [
        ("law clampAtMostHigh(int x, int low, int high) => Bounds.clamp(x, low, high) <= high;", true),
        ("law clampAtLeastLow(int x, int low, int high) when low <= high => Bounds.clamp(x, low, high) >= low;", true),
        ("law largestOfThree(int unused) => Bounds.largest(1, [5, 3]) == 5;", true),
        ("law clampAtMostLow(int x, int low, int high) => Bounds.clamp(x, low, high) <= low;", false),
    ] {
        let directory = library_project(&[("app/Shared/Bounds.sharp", &bounds(law))]);
        let root = directory.path();

        let output = compile(root);

        assert_eq!(output.status.success(), proven, "{law}: {}", printed(&output));
        if !proven {
            assert!(
                printed(&output).contains("error[law-unproven]: law clampAtMostLow is not proven"),
                "{}",
                printed(&output)
            );
        }
    }
}

/// Spec section 28: every law of a project with only `.sharp` files is proven, and no `.lean` file is in the project
/// before the compile or after it.
#[test]
fn compile_proves_spec_section_28_s_money_from_its_sharp_file_alone() {
    let directory = library_project(&[("app/Shared/Money.sharp", SPECIMEN_MONEY)]);
    let root = directory.path();
    assert_eq!(source_lean_files(root), Vec::<PathBuf>::new());

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shared/Money.sharp").exists());
    assert_eq!(source_lean_files(root), Vec::<PathBuf>::new());
}

/// Spec section 28: a `match` on an int parameter shows its bound as an `if` does: a comparison arm where it holds, and
/// in each arm after it, where it failed. The analyzer and the proof read one walk of the body's paths, so the call of
/// the law itself on `n - 1` after `<= 0` is smaller in both.
#[test]
fn compile_proves_a_law_whose_body_tests_its_bound_with_a_comparison_arm() {
    let money = SPECIMEN_MONEY.replace(
        "        if (n <= 0) {\n            return;\n        }\n        Money.timesKeepsCurrency(m, n - 1);\n        Money.addKeepsCurrency(m, m.times(n - 1));\n",
        "        match (n) {\n            <= 0 => { return; },\n            default => {\n                Money.timesKeepsCurrency(m, n - 1);\n                Money.addKeepsCurrency(m, m.times(n - 1));\n            },\n        }\n",
    );
    assert_ne!(money, SPECIMEN_MONEY);
    let directory = library_project(&[("app/Shared/Money.sharp", &money)]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shared/Money.sharp").exists());
}

/// Spec section 28: a case the body leaves open is `law-unproven`, with the case's conditions and the goal still to
/// show in PHP#, and the call of the law itself on a smaller value when the case makes none. `timesIsZero` holds where
/// `n <= 0` and is false where `n > 0`, whatever its body calls.
#[test]
fn a_case_a_law_s_body_leaves_open_names_its_conditions_and_its_goal_in_php_sharp() {
    let times_is_zero = |call: &str| {
        let law = format!(
            "\n    law timesIsZero(Money m, int n) => m.times(n).amount == 0\n    {{\n        if (n <= 0) {{\n            return;\n        }}\n{call}    }}\n\n    public static Money sum("
        );
        let money = SPECIMEN_MONEY.replacen("\n    public static Money sum(", &law, 1);
        assert_ne!(money, SPECIMEN_MONEY);
        money
    };
    let with_call = times_is_zero("        Money.timesIsZero(m, n - 1);\n");
    let without_calls = times_is_zero("");
    let without_sum_calls = SPECIMEN_MONEY.replace(
        "                Money.sumKeepsCurrency(total.add(first), rest);\n                Money.addKeepsCurrency(total, first);\n",
        "",
    );
    for (source, expected) in [
        (
            without_sum_calls.as_str(),
            "app/Shared/Money.sharp:24:5: error[law-unproven]: law sumKeepsCurrency is not proven\n = when items is [Money first, ...List<Money> rest], still to show: Money.sum(total.add(first), rest).currency == total.currency\n = no small value breaks it, so it may be true: its proof body has to show this case\n = Help: the law for a smaller items is the call Money.sumKeepsCurrency(total, rest)\n",
        ),
        (
            with_call.as_str(),
            "app/Shared/Money.sharp:19:5: error[law-unproven]: law timesIsZero is not proven\n = when n > 0, still to show: m.add(m.times(n - 1)).amount == 0\n",
        ),
        (
            without_calls.as_str(),
            "app/Shared/Money.sharp:19:5: error[law-unproven]: law timesIsZero is not proven\n = when n > 0, still to show: m.add(m.times(n - 1)).amount == 0\n = fails for: m = new Money(1, \"\"), n = 1\n = Help: the law for a smaller n is the call Money.timesIsZero(m, n - 1)\n",
        ),
    ] {
        let directory = library_project(&[("app/Shared/Money.sharp", source)]);
        let root = directory.path();

        let output = compile(root);

        assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
        assert!(printed(&output).contains(expected), "{}", printed(&output));
        assert!(!compiled(root, "app/Shared/Money.sharp").exists());
    }
}

/// Spec section 28: every goal prints in PHP#. A goal whose `match` the case leaves open, as `Money.sum`'s on a list
/// it knows nothing of, prints as the claim the law writes.
#[test]
fn a_goal_with_a_match_the_case_leaves_open_prints_as_the_law_s_claim() {
    let money = SPECIMEN_MONEY.replace(
        "    public static int refundable",
        "    law sumIsEuro(Money total, List<Money> items) => Money.sum(total, items).currency == \"EUR\";\n\n    public static int refundable",
    );
    let directory = library_project(&[("app/Shared/Money.sharp", &money)]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "error[law-unproven]: law sumIsEuro is not proven\n = still to show: Money.sum(total, items).currency == \"EUR\"\n"
        ),
        "{}",
        printed(&output)
    );
}

/// Spec section 28: a law with `when` is called only where its condition holds, so a call where it is not known names
/// the condition still to show.
#[test]
fn a_call_of_a_when_law_where_its_condition_is_not_known_names_the_condition_to_show() {
    let money = SPECIMEN_MONEY.replace(
        "        => Money.refundable(paid, refunded) <= paid;\n",
        "        => Money.refundable(paid, refunded) <= paid;\n    law refundOfAnything(int p, int r) => p < 0 || Money.refundable(p, r) <= p\n    {\n        Money.neverAbovePaid(p, r);\n    }\n",
    );
    let directory = library_project(&[("app/Shared/Money.sharp", &money)]);
    let root = directory.path();

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "error[law-unproven]: law refundOfAnything is not proven\n = still to show for the call Money.neverAbovePaid(p, r): p >= 0 && r >= 0\n"
        ),
        "{}",
        printed(&output)
    );
}

/// Spec section 28: a call of a `when` law where the conditions on the path show each part of its condition proves,
/// with nothing left to show for the call.
#[test]
fn a_call_of_a_when_law_where_each_part_of_its_condition_is_known_proves() {
    let money = SPECIMEN_MONEY.replace(
        "        => Money.refundable(paid, refunded) <= paid;\n",
        "        => Money.refundable(paid, refunded) <= paid;\n    law refundOfKnown(int p, int r) => p < 0 || r < 0 || Money.refundable(p, r) <= p\n    {\n        if (p < 0) {\n            return;\n        }\n        if (r < 0) {\n            return;\n        }\n        Money.neverAbovePaid(p, r);\n    }\n",
    );
    let directory = library_project(&[("app/Shared/Money.sharp", &money)]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shared/Money.sharp").exists());
}

/// Spec section 28: a law's `when` condition and its body read a nullable parameter as its claim does, so a value the
/// condition or an `if` tests for null is a value.
#[test]
fn compile_proves_a_law_whose_condition_or_body_reads_a_nullable_parameter() {
    let directory = lawful_project(&[(
        "app/Shared/Box.sharp",
        "namespace App.Shared;\n\npublic class Box\n{\n    law somePositive(int? x) when x != null && x + 1 > 0 => true;\n    law anything(int x) => true;\n    law someAnything(int? x) => true\n    {\n        if (x != null) {\n            Box.anything(x);\n        }\n    }\n}\n",
    )]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shared/Box.sharp").exists());
}

/// A law that calls a law of a file with errors is not proven until those errors are fixed, which the error says, as it
/// says of any code of such a file a law reaches.
#[test]
fn a_call_of_a_law_whose_file_has_errors_names_the_errors_to_fix() {
    let money = LAWFUL_MONEY.replace(
        "    law addKeepsCurrency",
        "    public int broken() => \"text\";\n    law same(int x) => x == x;\n\n    law addKeepsCurrency",
    );
    let directory = lawful_project(&[
        ("app/Shared/Money.sharp", &money),
        (
            "app/Shared/Ledger.sharp",
            "namespace App.Shared;\n\npublic class Ledger\n{\n    law usesMoney(int x) => true\n    {\n        Money.same(x);\n    }\n}\n",
        ),
    ]);
    let root = directory.path();

    let output = compile_lines(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "app/Shared/Ledger.sharp:5:5:error - law-unproven: Law usesMoney is not proven: it reaches Money.same, whose file has errors. Fix them, then run vendor/bin/mago compile again."
        ),
        "{}",
        printed(&output)
    );
}

/// Spec section 28: laws that call each other in a cycle never end, so the call that closes the cycle is the error of
/// a law that calls itself on no smaller value.
#[test]
fn laws_that_call_each_other_in_a_cycle_are_refused() {
    let cycle = "namespace App.Shared;\n\npublic class Count\n{\n    law up(int x) => true\n    {\n        Count.down(x);\n    }\n    law down(int x) => true\n    {\n        Count.up(x);\n    }\n}\n";
    let directory = lawful_project(&[("app/Shared/Count.sharp", cycle)]);
    let root = directory.path();

    let output = compile_lines(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert_eq!(
        issue_lines(&output),
        [
            "app/Shared/Count.sharp:7:9:error - law-recursion: a law calls itself only on a smaller value",
            "app/Shared/Count.sharp:11:9:error - law-recursion: a law calls itself only on a smaller value",
        ],
        "{}",
        printed(&output)
    );
}

/// Spec section 28: a `match` in a law's body takes every arm a `match` takes, and each arm is a case that knows its
/// pattern and its `when` guard: a constant, a type pattern that names the value, an enum's cases, a comparison and
/// `default`. A case an arm leaves open names its pattern. Spec section 28's `sumKeepsCurrency` covers a list.
#[test]
fn a_law_body_s_match_splits_its_cases_on_every_arm_a_match_takes() {
    let status = "namespace App.Shared;\n\npublic enum Status\n{\n    case Open;\n    case Paid;\n\n    public int weight() => match (this) {\n        Status.Open => 1,\n        Status.Paid => 2,\n    };\n}\n";
    let signs = |arms: &str| {
        format!(
            "namespace App.Shared;\n\npublic class Signs\n{{\n    public static int sign(int n) => n > 5 ? 2 : (n < 0 ? 0 : 1);\n\n    law signBounded(int n) => Signs.sign(n) >= 0 && Signs.sign(n) <= 2\n    {{\n        match (n) {{\n{arms}        }}\n    }}\n    law weighs(Status s) => s.weight() > 0\n    {{\n        match (s) {{\n            Status.Open => {{ return; }},\n            Status.Paid => {{ return; }},\n        }}\n    }}\n}}\n"
        )
    };
    let proven = signs(
        "            int m when m > 5 => { return; },\n            < 0 => { return; },\n            0 => { return; },\n            default => { return; },\n",
    );
    let directory = lawful_project(&[("app/Shared/Signs.sharp", &proven), ("app/Shared/Status.sharp", status)]);
    let output = compile(directory.path());
    assert!(output.status.success(), "{}", printed(&output));

    let positive = signs("            int m when m > 5 => { return; },\n            default => { return; },\n")
        .replace(
            "law signBounded(int n) => Signs.sign(n) >= 0 && Signs.sign(n) <= 2",
            "law signBounded(int n) => Signs.sign(n) == 2",
        );
    let directory = lawful_project(&[("app/Shared/Signs.sharp", &positive), ("app/Shared/Status.sharp", status)]);
    let output = compile(directory.path());
    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(" = when !(n is int m && m > 5), still to show: (n > 5 ? 2 : n < 0 ? 0 : 1) == 2\n"),
        "{}",
        printed(&output)
    );
}

/// Spec section 28: an `if` in a law's body that compares a value with an enum's case is a case that knows the case,
/// with the case on either side of `==`, `!=` in the `else` branch, and a local that holds a parameter, as a `match`
/// arm is. The case reads the code each call of the claim reaches, as `weight` through `level` and `rank` through
/// `meets`, as a law with no body does.
#[test]
fn a_law_body_s_if_splits_its_cases_on_an_enum_s_cases() {
    let role = "namespace App.Auth;\n\npublic enum Role\n{\n    case Owner;\n    case Admin;\n    case Member;\n\n    public int level() => Role.weight(this);\n    private static int weight(Role role) => match (role) {\n        Role.Owner => 3,\n        Role.Admin => 2,\n        Role.Member => 1,\n    };\n    public static Role fromLevel(int level) => match (level) {\n        3 => Role.Owner,\n        2 => Role.Admin,\n        default => Role.Member,\n    };\n\n    law fromLevelUndoesLevel(Role role) => Role.fromLevel(role.level()) == role\n    {\n        if (role == Role.Owner) {\n            return;\n        }\n        if (role == Role.Admin) {\n            return;\n        }\n        if (role == Role.Member) {\n            return;\n        }\n    }\n    law fromLevelUndoesLevelOfEitherSide(Role role) => Role.fromLevel(role.level()) == role\n    {\n        const same = role;\n        if (Role.Owner == same) {\n            return;\n        }\n        if (role != Role.Admin) {\n            return;\n        } else {\n            return;\n        }\n    }\n    law fromLevelUndoesLevelByMatch(Role role) => Role.fromLevel(role.level()) == role\n    {\n        match (role) {\n            Role.Owner => { return; },\n            Role.Admin => { return; },\n            Role.Member => { return; },\n        }\n    }\n}\n";
    let level = "namespace App.Auth;\n\npublic enum Level\n{\n    case Low;\n    case Mid;\n    case High;\n\n    private static int rank(Level level) => match (level) {\n        Level.Low => 1,\n        Level.Mid => 2,\n        Level.High => 3,\n    };\n    public bool meets(Level @required) => Level.rank(this) >= Level.rank(@required);\n\n    law sameWhenEachMeetsTheOther(Level a, Level b) => !(a.meets(b) && b.meets(a)) || a == b\n    {\n        if (b == Level.Low) {\n            return;\n        }\n        if (b == Level.Mid) {\n            return;\n        }\n        if (b == Level.High) {\n            return;\n        }\n    }\n}\n";
    let directory = lawful_project(&[("app/Auth/Role.sharp", role), ("app/Auth/Level.sharp", level)]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Auth/Role.sharp").exists());
    assert!(compiled(root, "app/Auth/Level.sharp").exists());
}

/// A law's proof splits its cases on a value it can name without running code, so a `match` of any other value is
/// refused with what to match instead.
#[test]
fn a_law_body_s_match_on_a_value_that_runs_code_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    law signs(int n) => true\n    {\n        match (n + 1) {\n            0 => { return; },\n            default => { return; },\n        }\n    }\n}\n",
        "Law signs reaches a match on `n + 1` in Shape.signs. Match a parameter of the law, a value a pattern of its body names, or a property of one.",
    );
}

/// Spec section 28: a law reads a `List` with the members whose model the checker has, and a member it has none of is
/// refused by its name, with those members named.
#[test]
fn a_law_reaching_another_list_method_is_refused_naming_the_members_it_reads() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public static List<int> turned(List<int> items) => items.reversed();\n\n    law turns(List<int> items) => Shape.turned(items).count() == items.count();\n}\n",
        "Law turns reaches the List method reversed in Shape.turned. Hold the values in a List, and read it with `count()`, `isEmpty()`, `isNotEmpty()`, `map`, `filter`, `join`, `any`, `all`, an index, a list pattern or a spread.",
    );
}

/// Spec section 28: a lambda passed to a `List`'s `map`, `filter`, `any` or `all`, or a `Map`'s `filter` or `any`, may
/// call PHP# methods, as `cases()`, `tryFrom` and a static method that compares, so a law proves that no value of one
/// string enum is a value of another.
#[test]
fn a_law_lambda_may_call_a_method_that_cannot_throw() {
    let directory = library_project(&[
        (
            "app/Kinds/Color.sharp",
            "namespace App.Kinds;\n\npublic enum Color : string\n{\n    case Red = \"red\";\n    case Blue = \"blue\";\n}\n",
        ),
        (
            "app/Kinds/Mood.sharp",
            "namespace App.Kinds;\n\npublic enum Mood : string\n{\n    case Calm = \"calm\";\n    case Angry = \"angry\";\n}\n",
        ),
        (
            "app/Kinds/Kinds.sharp",
            "namespace App.Kinds;\n\npublic static class Kinds\n{\n    public static bool clashes(Color color) => Mood.cases().any(mood => mood.value == color.value);\n\n    public static bool same(Color color, Mood mood) => color.value == mood.value;\n\n    law noColorIsAMood() => Color.cases().filter(color => Mood.cases().filter(mood => mood.value == color.value).isNotEmpty()).isEmpty();\n\n    law noColorClashes() => Color.cases().filter(color => Kinds.clashes(color)).isEmpty();\n\n    law noColorReadsAsAMood() => Color.cases().filter(color => Mood.tryFrom(color.value) != null).isEmpty();\n\n    law noMoodIsThisColor(Color color) => Mood.cases().filter(mood => Kinds.same(color, mood)).isEmpty();\n\n    law aMapKeepsNoClash() => [\"a\": Color.Red, \"b\": Color.Blue].filter(color => Kinds.clashes(color)).isEmpty();\n\n    law aMapHasNoClash() => ![\"a\": Color.Red].any(color => Kinds.clashes(color));\n}\n",
        ),
    ]);
    let root = directory.path();

    let output = compile_lines(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Kinds/Kinds.sharp").exists());
}

/// Spec section 28: a lambda whose body can throw, as `x * 2` throws on overflow, translates as a function of the monad
/// the code runs in, so its law is judged on its own terms: `same` is false for a list whose doubling overflows, so it
/// is not proved, and no construct refuses it.
#[test]
fn a_law_lambda_that_can_throw_translates() {
    let directory = library_project(&[(
        "app/Shared/Shape.sharp",
        "namespace App.Shared;\n\npublic class Shape\n{\n    public static List<int> doubled(List<int> items) => items.map(x => x * 2);\n\n    law same(List<int> items) => Shape.doubled(items).count() == items.count();\n}\n",
    )]);
    let root = directory.path();

    let output = compile_lines(root);

    assert!(!printed(&output).contains("unmodeled-construct"), "{}", printed(&output));
    assert!(printed(&output).contains("error - law-unproven: law same"), "{}", printed(&output));
    assert!(!compiled(root, "app/Shared/Shape.sharp").exists());
}

#[test]
fn a_law_reaching_a_loop_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public static int sum(int n)\n    {\n        let total = 0;\n        for (let i = 0; i < n; i++) {\n            total += i;\n        }\n        return total;\n    }\n\n    law summed(int n) => Shape.sum(n) >= 0;\n}\n",
        "Law summed reaches a loop in Shape.sum. Write the method as recursion that calls itself on a smaller value.",
    );
}

#[test]
fn a_law_reaching_a_while_loop_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public static int steps(int n)\n    {\n        let left = n;\n        let steps = 0;\n        while (left > 0) {\n            left -= 1;\n            steps += 1;\n        }\n        return steps;\n    }\n\n    law counted(int n) => Shape.steps(n) >= 0;\n}\n",
        "Law counted reaches a loop in Shape.steps. Write the method as recursion that calls itself on a smaller value.",
    );
}

/// A `for … of` loop is a law's only when it only adds to int locals, so one that adds to a string is refused.
#[test]
fn a_law_reaching_a_loop_that_writes_a_string_local_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public static string joined(List<string> names)\n    {\n        let text = \"\";\n        for (const name of names) {\n            text += name;\n        }\n        return text;\n    }\n\n    law joinedText(List<string> names) => Shape.joined(names) == Shape.joined(names);\n}\n",
        "Law joinedText reaches a loop that does more than count or sum in Shape.joined. Write the method as recursion that calls itself on a smaller value.",
    );
}

/// `n + 1` under `n <= 0` moves away from the bound the condition tested, so the recursion never gets smaller.
#[test]
fn a_law_reaching_a_recursion_that_does_not_get_smaller_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public static int up(int n) => n <= 0 ? 0 : Shape.up(n + 1);\n\n    law reachesZero(int n) => Shape.up(n) == 0;\n}\n",
        "Law reachesZero reaches a recursion that does not get smaller in Shape.up. Write the method as recursion that calls itself on a smaller value: a list's rest from a list pattern, or an int one step towards a bound a condition tested.",
    );
}

/// Mutual recursion ends only when every call among the methods passes a smaller value, so a call on `n` itself is
/// refused.
#[test]
fn a_law_reaching_a_mutual_recursion_that_does_not_get_smaller_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public static bool isEven(int n) => n <= 0 ? true : Shape.isOdd(n - 1);\n    public static bool isOdd(int n) => n <= 0 ? false : Shape.isEven(n);\n\n    law fourIsEven(int unused) => Shape.isEven(4);\n}\n",
        "Law fourIsEven reaches a recursion that does not get smaller in Shape.isOdd. Write the method as recursion that calls itself on a smaller value: a list's rest from a list pattern, or an int one step towards a bound a condition tested.",
    );
}

/// A class whose property holds the class is a Lean structure that holds itself, which Lean refuses.
#[test]
fn a_law_reaching_a_type_that_holds_itself_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public Shape(public int value { get; }, public Shape? next { get; }) { }\n\n    law keepsValue(Shape shape) => shape.value == shape.value;\n}\n",
        "Law keepsValue reaches a type that holds itself in Shape. A law reasons only about a class whose properties never hold a value of the class itself.",
    );
}

/// A value pattern tests a List by `==`, which a `when` guard writes, so the refusal says to write it there.
#[test]
fn a_law_reaching_a_value_pattern_on_a_list_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public const List<int> PAIR = [1, 2];\n\n    public static int pair(List<int> xs) => match (xs) {\n        Shape.PAIR => 1,\n        default => 0,\n    };\n\n    law pairHolds(List<int> xs) => Shape.pair(xs) == Shape.pair(xs);\n}\n",
        "Law pairHolds reaches a match on this subject in Shape.pair. Compare the value with `==` in a `when` guard.",
    );
}

/// Each refusal a law reaches names the construct and says, in one PHP# sentence, what to write in its place, or why
/// the value is out of a law's reach.
#[test]
fn each_refusal_a_law_reaches_says_what_to_write_instead() {
    let source = "namespace App.Shared;\n\npublic class Shape\n{\n    public Shape(public int value { get; }) { }\n    public static bool operator ==(Shape a, Shape b) => a.value == b.value;\n    public int hash() => this.value;\n\n    public static int union(int|string x) => 1;\n    public static int power(int a) => a ** 2;\n    public static int complement(int a) => ~a;\n    public static string template(bool a) => `n ${a}`;\n    public static int guard(int a) => match (a) {\n        int m when m + 1 > 2 => 1,\n        default => 0,\n    };\n    public static int defaulted(int a = 1) => a;\n    public static int named(int a) => Shape.defaulted(a: a);\n    public static int compound(int a)\n    {\n        let x = a;\n        x **= 2;\n        return x;\n    }\n    public static int inner(int a)\n    {\n        let x = 0;\n        return (x = a) + x;\n    }\n\n    law l1(int a) => Shape.union(a) == 1;\n    law l2(int a) => Shape.power(a) == Shape.power(a);\n    law l3(int a) => Shape.complement(a) == Shape.complement(a);\n    law l4(bool a) => Shape.template(a) == Shape.template(a);\n    law l5(int a) => Shape.guard(a) == Shape.guard(a);\n    law l6(int a) => Shape.named(a) == a;\n    law l7(int a) => Shape.compound(a) == Shape.compound(a);\n    law l8(int a) => Shape.inner(a) == Shape.inner(a);\n}\n";
    let directory = lawful_project(&[("app/Shared/Shape.sharp", source)]);
    let root = directory.path();

    let output = compile_without_lean(root);

    let printed = printed(&output);
    assert_eq!(output.status.code(), Some(1), "{printed}");
    for message in [
        "Law l1 reaches the type `int|string` in Shape.union. Give it a type a law reasons about: int, bool, string, a List, a Map, or a PHP# class, enum or interface, with or without `?`.",
        "Law l2 reaches the operator `**` here in Shape.power. Write it with the operators a law reaches: `+ - * / %` on ints, `+` on strings, `< <= > >=` on ints, on strings and on two values of a class that declares `<=>`, `<=>` on ints and on those values, `== !=`, `! && ||` and `??`.",
        "Law l3 reaches the operator `~` in Shape.complement. Write it with the operators a law reaches: `+ - * / %` on ints, `+` on strings, `< <= > >=` on ints, on strings and on two values of a class that declares `<=>`, `<=>` on ints and on those values, `== !=`, `! && ||` and `??`.",
        "Law l4 reaches a string template in Shape.template. Join the strings with `+`.",
        "Law l5 reaches a guard that can throw in Shape.guard. Write the guard without calls or arithmetic, which can throw, and compute such a value in the arm.",
        "Law l6 reaches a named or spread argument in Shape.named. Pass each argument by its position.",
        "Law l7 reaches the operator `**=` in Shape.compound. Update an int with `+=`, `-=`, `*=`, `/=` or `%=`, or a string with `+=`.",
        "Law l8 reaches an assignment inside an expression in Shape.inner. Assign the value in a statement of its own, before the expression that reads it.",
    ] {
        assert!(printed.contains(&format!("error - unmodeled-construct: {message}")), "{message}\n{printed}");
    }
    assert!(!printed.contains("A law cannot reason about it."), "{printed}");
}

/// The refusals of code a law cannot read say what to write instead, and name a type parameter as one and a function a
/// library declares in PHP's root namespace through the namespace of its classes, never as a plain PHP class.
#[test]
fn each_refusal_of_code_a_law_cannot_read_says_what_to_write_instead() {
    let clock = (
        "vendor/lib/clock/Clock.php",
        "<?php\n\nnamespace Lib;\n\nfinal class Clock\n{\n    public static function hour(): int\n    {\n        return 1;\n    }\n}\n",
    );
    let helpers = (
        "vendor/lib/clock/helpers.php",
        "<?php\n\nfunction measure(string $text): int\n{\n    return strlen($text);\n}\n",
    );
    let stub = (
        "app/Stubs/Clock.sharp",
        "namespace App.Stubs;\n\nimport Lib;\nimport Lib.Clock;\n\nextern namespace Lib for measure;\nextern Clock uses none;\nextern Lib.measure uses none;\n",
    );
    let shape = (
        "app/Shared/Shape.sharp",
        "namespace App.Shared;\n\nimport Lib;\nimport Lib.Clock;\n\npublic class Shape\n{\n    public Shape(public int raw { get; }) { }\n    public int scaled { get => field; set => field = value; } = 0;\n\n    public static int hour(int a) => Clock.hour();\n    public static int length(string s) => Lib.measure(s);\n    public static int kept<T>(T item) => 1;\n\n    law l1(int a) => Shape.hour(a) == Shape.hour(a);\n    law l2(string s) => Shape.length(s) == Shape.length(s);\n    law l3(int a) => Shape.kept(a) == 1;\n    law l4(Shape s) => s.scaled == s.scaled;\n}\n",
    );
    let directory = library_project(&[shape, clock, helpers, stub]);
    let root = directory.path();

    let output = compile_without_lean(root);

    let printed = printed(&output);
    assert_eq!(output.status.code(), Some(1), "{printed}");
    for message in [
        "Law l1 reaches the plain PHP class Clock in Shape.hour. Write the code the law reaches in PHP#, or pass the value plain PHP computes to the law as a parameter.",
        "Law l2 reaches the plain PHP function `Lib.measure` in Shape.length. Write the code the law reaches in PHP#, or pass the value plain PHP computes to the law as a parameter.",
        "Law l3 reaches the type parameter T in Shape.kept. Give it a type a law reasons about: int, bool, string, a List, a Map, or a PHP# class, enum or interface, with or without `?`.",
        "Law l4 reaches the accessor of Shape.scaled in Shape.l4. Compute the value in a method, and give the property no accessor body.",
    ] {
        assert!(printed.contains(&format!("error - unmodeled-construct: {message}")), "{message}\n{printed}");
    }
    assert!(!printed.contains("the plain PHP class T"), "{printed}");
}

/// A loop that reads the local it adds to, here in an `if`, does more than count or sum.
#[test]
fn a_law_reaching_a_loop_that_reads_a_local_it_adds_to_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public static int capped(List<int> xs)\n    {\n        let sum = 0;\n        for (const x of xs) {\n            if (sum < 10) {\n                sum += x;\n            }\n        }\n        return sum;\n    }\n\n    law cappedHolds(List<int> xs) => Shape.capped(xs) == Shape.capped(xs);\n}\n",
        "Law cappedHolds reaches a loop that reads a local it adds to in Shape.capped. Write the method as recursion that calls itself on a smaller value.",
    );
}

/// PHP finds a property by its exact name, so a law knows a stored `raw` and a computed `Raw` apart.
#[test]
fn a_law_knows_a_stored_field_from_a_computed_property_whose_name_differs_only_in_case() {
    let directory = lawful_project(&[(
        "app/Shared/Shape.sharp",
        "namespace App.Shared;\n\npublic class Shape\n{\n    public Shape(public int raw { get; }) { }\n    public int Raw => this.raw * 2;\n\n    law doubledHolds(Shape s) => s.Raw == s.raw * 2;\n}\n",
    )]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shared/Shape.sharp").exists());
}

#[test]
fn a_law_reaching_a_pattern_that_names_a_value_outside_an_if_or_a_conditional_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public Shape(public int total { get; }) { }\n\n    public static bool big(Shape? shape) => shape is Shape found && found.total > 10;\n\n    law nullIsSmall(int unused) => !Shape.big(null);\n}\n",
        "Law nullIsSmall reaches a pattern that names a value outside the condition of an if or a ?: in Shape.big. Write the test as the condition of an if or a ?:, and read the value it names in the branch it decides.",
    );
}

#[test]
fn a_law_reaching_a_list_pattern_on_a_value_that_is_no_list_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public static int first(Any? value) => match (value) {\n        [int first, ...] => first,\n        default => 0,\n    };\n\n    law firstOfNull(int unused) => Shape.first(null) == 0;\n}\n",
        "Law firstOfNull reaches a list pattern on a value that is no List in Shape.first. It reads types while the code runs.",
    );
}

#[test]
fn a_law_reaching_a_lambda_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public static int next(int a)\n    {\n        const step = (int x) => x + 1;\n        return step(a);\n    }\n\n    law grows(int a) => Shape.next(a) != a;\n}\n",
        "Law grows reaches a lambda in Shape.next. Pass a lambda only to a List's `map` or `filter`, with a body that cannot throw, and call a PHP# method in place of any other function value.",
    );
}

/// Spec section 28: a value whose class extends PHP# classes holds their fields, up to the first plain PHP class above
/// it, whose state a law carries and never reads. A law carries such a value, and reads what its PHP# classes declare.
#[test]
fn compile_proves_a_law_over_a_value_whose_class_extends_php_sharp_and_plain_php() {
    let holder = |laws: &str| {
        format!(
            "namespace App.Shared;\n\npublic struct Holder\n{{\n    public Holder(public Entry entry {{ get; }}, public int at {{ get; }}) {{ }}\n\n    {laws}\n}}\n"
        )
    };
    let family = |laws: &str| {
        lawful_project(&[
            (
                "vendor/acme/orm/src/Base.php",
                "<?php\n\nnamespace Acme\\Orm;\n\nclass Base\n{\n    public int $x = 0;\n}\n",
            ),
            (
                "app/Shared/Stamped.sharp",
                "namespace App.Shared;\n\nimport Acme.Orm.Base;\n\npublic class Stamped : Base\n{\n    public Stamped(public int created { get; }) : super() { }\n}\n",
            ),
            (
                "app/Shared/Entry.sharp",
                "namespace App.Shared;\n\npublic class Entry : Stamped\n{\n    public Entry(public int id { get; }, int created) : super(created) { }\n}\n",
            ),
            ("app/Shared/Holder.sharp", &holder(laws)),
        ])
    };
    let directory = family(
        "law carries(Entry entry, int at) => new Holder(entry, at).at == at;\n    law readsOwn(Entry entry, int at) => new Holder(entry, at).entry.id == entry.id;\n    law readsPhpSharpParent(Entry entry, int at) => new Holder(entry, at).entry.created == entry.created;",
    );
    let root = directory.path();

    compile_succeeds(root);
    assert!(compiled(root, "app/Shared/Holder.sharp").exists());

    let directory = family("law parentIsOwn(Entry entry) => entry.created == entry.id;");

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(printed(&output).contains("error[law-unproven]: law parentIsOwn is not proven\n"), "{}", printed(&output));
}

/// Spec section 28: a class's structure holds the fields of every PHP# class above it, so a law reads a field, calls a
/// method and runs a constructor that a class two levels up declares, through a value of the class at the bottom, a
/// sealed interface's implementer included.
#[test]
fn compile_proves_a_law_through_a_chain_of_php_sharp_classes() {
    let line = |laws: &str| {
        format!(
            "namespace App.Shared;\n\npublic class Line : Item\n{{\n    public Line(public int id {{ get; }}, int cents) : super(cents) {{ }}\n\n    {laws}\n}}\n"
        )
    };
    let chain = |laws: &str| {
        lawful_project(&[
            (
                "app/Shared/Priced.sharp",
                "namespace App.Shared;\n\npublic class Priced\n{\n    public Priced(public int cents { get; }) { }\n\n    public int doubled() => this.cents * 2;\n}\n",
            ),
            (
                "app/Shared/Item.sharp",
                "namespace App.Shared;\n\npublic class Item : Priced\n{\n    public Item(int cents) : super(cents) { }\n}\n",
            ),
            ("app/Shared/Charge.sharp", "namespace App.Shared;\n\npublic sealed interface Charge\n{\n}\n"),
            (
                "app/Shared/Fee.sharp",
                "namespace App.Shared;\n\npublic final class Fee : Item, Charge\n{\n    public Fee(int cents) : super(cents) { }\n}\n",
            ),
            ("app/Shared/Line.sharp", &line(laws)),
        ])
    };
    let directory = chain(
        "law readsTop(Line line) => line.cents == line.cents;\n    law builds(int id, int cents) => new Line(id, cents).cents == cents;\n    law calls(Line line) => line.doubled() == line.cents * 2;\n    law feeReadsTop(int cents) => new Fee(cents).cents == cents;\n    law feeCalls(int cents) => new Fee(cents).doubled() == cents * 2;",
    );
    let root = directory.path();

    compile_succeeds(root);
    assert!(compiled(root, "app/Shared/Line.sharp").exists());

    let directory = chain("law swapped(int id, int cents) => new Line(id, cents).cents == id;");

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(printed(&output).contains("error[law-unproven]: law swapped is not proven\n"), "{}", printed(&output));
}

/// Spec section 28: a member a plain PHP class above a value's class declares is plain PHP, so a law that reads it is
/// refused at the read, and a `new` that runs a plain PHP constructor is refused at the `new`.
#[test]
fn a_law_reading_what_a_plain_php_parent_declares_is_refused_where_it_reads_it() {
    let directory = lawful_project(&[
        (
            "vendor/acme/orm/src/Base.php",
            "<?php\n\nnamespace Acme\\Orm;\n\nclass Base\n{\n    public int $x = 0;\n\n    public function __construct()\n    {\n    }\n\n    final public function size(): int\n    {\n        return 1;\n    }\n}\n",
        ),
        (
            "app/Shared/Shape.sharp",
            "namespace App.Shared;\n\nimport Acme.Orm.Base;\n\nextern Base uses none;\n\npublic class Shape : Base\n{\n    public Shape(public int sides { get; }) : super() { }\n\n    law readsBase(Shape shape) => shape.x == shape.x;\n    law callsBase(Shape shape) => shape.size() == shape.size();\n    law builds(int sides) => new Shape(sides).sides == sides;\n}\n",
        ),
    ]);
    let root = directory.path();

    let output = compile_without_lean(root);

    let printed = printed(&output);
    assert_eq!(output.status.code(), Some(1), "{printed}");
    for message in [
        "Law readsBase reaches the plain PHP property Base.x in Shape.readsBase. Write the code the law reaches in PHP#, or pass the value plain PHP computes to the law as a parameter.",
        "Law callsBase reaches the plain PHP method Base.size in Shape.callsBase. Write the code the law reaches in PHP#, or pass the value plain PHP computes to the law as a parameter.",
        "Law builds reaches the constructor of the plain PHP class Base in Shape.builds. Write the code the law reaches in PHP#, or pass the value plain PHP computes to the law as a parameter.",
    ] {
        assert!(printed.contains(&format!("error - unmodeled-construct: {message}")), "{message}\n{printed}");
    }
}

#[test]
fn a_law_reaching_a_named_constructor_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public Shape(public int sides { get; }) { }\n    public Shape.triangle() : this(3) { }\n\n    law threeSides(int unused) => new Shape.triangle().sides == 3;\n}\n",
        "Law threeSides reaches the named constructor Shape.triangle in Shape.threeSides. Write the constructor with promoted properties and a body that only checks its arguments and throws, and compute the values in a static method that calls `new`.",
    );
}

/// Spec section 28: a constructor body that does more than check its arguments and throw is refused, as one that
/// declares a local before its check.
#[test]
fn a_law_reaching_a_constructor_body_that_does_more_than_check_and_throw_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\nimport ValueError;\n\npublic class Shape\n{\n    public Shape(public int sides { get; })\n    {\n        const least = 3;\n        if (sides < least) {\n            throw new ValueError(\"A shape has 3 sides or more.\");\n        }\n    }\n\n    law threeSides(int unused) => new Shape(3).sides == 3;\n}\n",
        "Law threeSides reaches the body of the constructor of Shape in new Shape. Write the constructor with promoted properties and a body that only checks its arguments and throws, and compute the values in a static method that calls `new`.",
    );
}

#[test]
fn a_law_reaching_an_extension_member_lean_cannot_see_is_refused() {
    assert_unmodeled_beside(
        "namespace App.Shared;\n\npublic final class Shape\n{\n    public Shape(public int sides { get; }) { }\n\n    law summed(Shape s) => s.sum() >= 0;\n}\n",
        &[(
            "app/Shared/ShapeMath.sharp",
            "namespace App.Shared;\n\npublic static class ShapeMath\n{\n    extension(Shape shape)\n    {\n        public int sum()\n        {\n            let total = 0;\n            for (let i = 0; i < shape.sides; i++) {\n                total += i;\n            }\n            return total;\n        }\n    }\n}\n",
        )],
        "Law summed reaches a loop in ShapeMath.sum. Write the method as recursion that calls itself on a smaller value.",
    );
}

/// `app/Readiness.sharp`, a machine whose final case's block holds a computed property over the case's data.
const READINESS: &str = "namespace App;\n\npublic machine Readiness\n{\n    case Active\n    {\n        public Readiness.Inactive stop(string reason) => Readiness.Inactive(reason);\n    }\n\n    final case Inactive(string reason)\n    {\n        public string failureMessage => this.reason;\n    }\n\n    public static Readiness.Active open() => Readiness.Active;\n}\n";

/// Spec section 20.2: a computed property in a case's block is a member of that case's class, so the machine compiles.
#[test]
fn a_computed_property_in_a_machine_case_s_block_compiles() {
    let directory = library_project(&[("app/Readiness.sharp", READINESS)]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Readiness.sharp").exists());
}

/// Spec section 20.2: a law reads a computed property of a case's block on a value of the case.
#[test]
fn compile_proves_a_law_over_a_computed_property_in_a_machine_case_s_block() {
    let law = "    law readsItsReason(string reason) => Readiness.open().stop(reason).failureMessage == reason;\n}\n";
    let machine = format!("{}\n{law}", READINESS.strip_suffix("}\n").unwrap_or(READINESS));
    let directory = library_project(&[("app/Readiness.sharp", &machine)]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Readiness.sharp").exists());
}

/// `app/Notice.sharp`, a class whose computed property joins a list of strings, with `laws` in its body.
fn notice(laws: &str) -> String {
    format!(
        "namespace App;\n\npublic final class Notice\n{{\n    public Notice(public string reason {{ get; }}, public List<string> steps {{ get; }}) {{ }}\n\n    public string checklist => this.steps.join(\", \");\n\n{laws}}}\n"
    )
}

/// A law states the text `join` builds: the strings with the separator between each two, and nothing of no strings.
#[test]
fn compile_proves_a_law_over_a_joined_list_of_strings() {
    let laws = "    law twoStepsJoin(string a, string b) => new Notice(\"\", [a, b]).checklist == `${a}, ${b}`;\n\n    law emptyJoinsEmpty() => new Notice(\"\", []).checklist == \"\";\n";
    let directory = library_project(&[("app/Notice.sharp", &notice(laws))]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Notice.sharp").exists());
}

/// A law that states the wrong text of a join is not proven, and names the small values that break it.
#[test]
fn a_false_law_over_a_join_is_not_proven() {
    let laws = "    law joinsWithoutSpace(string a, string b) => [a, b].join(\", \") == `${a},${b}`;\n";
    let directory = library_project(&[("app/Notice.sharp", &notice(laws))]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains("error[law-unproven]: law joinsWithoutSpace is not proven\n"),
        "{}",
        printed(&output)
    );
    assert!(printed(&output).contains(" = fails for: a = \"\", b = \"\"\n"), "{}", printed(&output));
}

const VISIT: &str = "namespace App.Visits;\n\nimport Sharp.Time.Duration;\nimport Sharp.Time.Instant;\n\npublic machine Visit\n{\n    final case Live(Instant idleAt, Instant endsAt);\n    final case Idle(Instant endsAt);\n    final case Ended;\n\n    public const int IDLE_MINUTES = 5;\n    public const int LIFETIME_MINUTES = 30;\n    public const int MAX_HOURS = 24;\n\n    internal static Visit.Live seen(Instant started, Instant lastSeen)\n    {\n        const lapses = lastSeen.plus(Duration.ofMinutes(Visit.LIFETIME_MINUTES));\n        const ends = started.plus(Duration.ofHours(Visit.MAX_HOURS));\n        return Visit.Live(lastSeen.plus(Duration.ofMinutes(Visit.IDLE_MINUTES)), lapses < ends ? lapses : ends);\n    }\n\n    internal static Visit at(Visit status, Instant now)\n        => status is Visit.Live live\n            ? (now >= live.endsAt ? Visit.Ended : now >= live.idleAt ? Visit.Idle(live.endsAt) : live)\n            : status is Visit.Idle idle\n                ? (now >= idle.endsAt ? Visit.Ended : idle)\n                : status;\n\n    law aVisitEndsItsLifetimeAfterTheSighting(Instant started, Instant lastSeen)\n        when lastSeen.plus(Duration.ofMinutes(Visit.LIFETIME_MINUTES)) < started.plus(Duration.ofHours(Visit.MAX_HOURS))\n        => Visit.seen(started, lastSeen).endsAt == lastSeen.plus(Duration.ofMinutes(Visit.LIFETIME_MINUTES));\n    law aVisitNeverOutlivesItsLongestLength(Instant started, Instant lastSeen)\n        when lastSeen.plus(Duration.ofMinutes(Visit.LIFETIME_MINUTES)) >= started.plus(Duration.ofHours(Visit.MAX_HOURS))\n        => Visit.seen(started, lastSeen).endsAt == started.plus(Duration.ofHours(Visit.MAX_HOURS));\n    law aSightingBeforeTheEndStaysInTheVisit(Instant started, Instant lastSeen, Instant now)\n        when now < Visit.seen(started, lastSeen).endsAt\n        => !(Visit.at(Visit.seen(started, lastSeen), now) is Visit.Ended);\n    law aSightingAtTheEndOrAfterEndsTheVisit(Instant started, Instant lastSeen, Instant now)\n        when now >= Visit.seen(started, lastSeen).endsAt\n        => Visit.at(Visit.seen(started, lastSeen), now) is Visit.Ended;\n}\n";

/// A law over `Instant.plus` of a `Duration` a constant gives, compared with another `Instant`, proves within its time,
/// and so does a law over the case a `?:` between two `Instant`s chooses.
#[test]
fn a_law_over_instant_plus_a_constant_duration_proves() {
    let directory = library_project(&[("app/Visits/Visit.sharp", VISIT)]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Visits/Visit.sharp").exists());
}

const RECEIPT: &str = "namespace App.Store;\n\npublic class Receipt\n{\n    public Receipt(\n        public string holder { get; set; },\n        public string? gateway { get; set; } = null,\n        public string? last4 { get; set; } = null,\n        public string? brand { get; set; } = null,\n        public string? transactionId { get; set; } = null,\n        public bool isTest { get; set; } = false,\n    ) : super() {\n    }\n}\n";

const VARIANT: &str = "namespace App.Store;\n\npublic class Variant\n{\n}\n";

/// `app/Store/OrderStatus.sharp`, a machine of nine cases whose moves pay, refund and dispute an order, with `laws` and
/// the invariant `aPartlyRefundedOrderHoldsARefund` in its body, which reads `Receipt` and `Variant`.
fn refunding_order(laws: &str) -> String {
    format!(
        r#"namespace App.Store;

import Sharp.Key;
import Sharp.Tag;

[Tag("status")]
public machine OrderStatus : Variant
{{
    [Key("pending")] case Pending([Key("payment")] Receipt payment)
    {{
        public OrderStatus.Paid pay(Receipt payment) => OrderStatus.Paid(payment);
        public OrderStatus.Reserved reserve(Receipt payment) => OrderStatus.Reserved(payment);
        public OrderStatus.Failed fail(string? failureReason) => OrderStatus.Failed(failureReason, this.payment);
        public OrderStatus.Abandoned abandon(string? failureReason) => OrderStatus.Abandoned(failureReason, this.payment);
        public OrderStatus.Canceled cancel(string? failureReason) => OrderStatus.Canceled(failureReason, this.payment);
    }}
    [Key("reserved")] case Reserved([Key("payment")] Receipt payment)
    {{
        public OrderStatus.Paid pay(Receipt payment) => OrderStatus.Paid(payment);
        public OrderStatus.Failed fail(string? failureReason) => OrderStatus.Failed(failureReason, this.payment);
    }}
    [Key("paid")] case Paid([Key("payment")] Receipt payment)
    {{
        public OrderStatus.PartiallyRefunded|OrderStatus.Refunded refund(int cents, int amount)
        {{
            if (cents <= 0 || cents > amount) {{ throw new ValueError(`A refund takes between 1 and ${{amount}}, not ${{cents}}.`); }}
            return cents == amount ? OrderStatus.Refunded(this.payment) : OrderStatus.PartiallyRefunded(cents, this.payment);
        }}
        public OrderStatus.Disputed dispute() => OrderStatus.Disputed(0, this.payment);
        public OrderStatus.Paid pay(Receipt payment) => this;
    }}
    [Key("partiallyRefunded")] case PartiallyRefunded([Key("refunded_amount")] int refundedCents, [Key("payment")] Receipt payment)
    {{
        public OrderStatus.PartiallyRefunded|OrderStatus.Refunded refund(int cents, int amount)
        {{
            if (cents <= 0 || this.refundedCents + cents > amount) {{ throw new ValueError(`A refund takes between 1 and ${{amount - this.refundedCents}}, not ${{cents}}.`); }}
            return this.refundedCents + cents == amount ? OrderStatus.Refunded(this.payment) : OrderStatus.PartiallyRefunded(this.refundedCents + cents, this.payment);
        }}
        public OrderStatus.Disputed dispute()
            => this.refundedCents >= 0 ? OrderStatus.Disputed(this.refundedCents, this.payment) : throw new ValueError(`${{this.refundedCents}} is below 0.`);
        public OrderStatus.PartiallyRefunded pay(Receipt payment) => this;
    }}
    [Key("disputed")] case Disputed([Key("refunded_amount")] int refundedCents, [Key("payment")] Receipt payment)
    {{
        public OrderStatus.Paid|OrderStatus.PartiallyRefunded win()
            => this.refundedCents > 0 ? OrderStatus.PartiallyRefunded(this.refundedCents, this.payment) : OrderStatus.Paid(this.payment);
        public OrderStatus.Refunded lose(int cents, int amount)
            => this.refundedCents + cents == amount ? OrderStatus.Refunded(this.payment) : throw new ValueError(`A lost dispute takes all ${{amount - this.refundedCents}}, not ${{cents}}.`);
        public OrderStatus.Disputed pay(Receipt payment) => this;
    }}
    [Key("failed")] case Failed([Key("failure_reason")] string? failureReason, [Key("payment")] Receipt payment)
    {{ public OrderStatus.Paid pay(Receipt payment) => OrderStatus.Paid(payment); }}
    [Key("abandoned")] case Abandoned([Key("failure_reason")] string? failureReason, [Key("payment")] Receipt payment)
    {{ public OrderStatus.Paid pay(Receipt payment) => OrderStatus.Paid(payment); }}
    [Key("canceled")] case Canceled([Key("failure_reason")] string? failureReason, [Key("payment")] Receipt payment)
    {{
        public OrderStatus.Canceled receiveLatePayment(Receipt payment) => OrderStatus.Canceled(this.failureReason, payment);
        public OrderStatus.Refunded refund(int cents, int amount)
            => cents == amount ? OrderStatus.Refunded(this.payment) : throw new ValueError(`Refunded only in full, ${{amount}}, not ${{cents}}.`);
    }}
    [Key("refunded")] case Refunded([Key("payment")] Receipt payment)
    {{ public OrderStatus.Refunded pay(Receipt payment) => this; }}

    public static OrderStatus.Pending placed(Receipt payment) => OrderStatus.Pending(payment);

    public int refunded(int amount) => match (this) {{
        OrderStatus.PartiallyRefunded partly => partly.refundedCents,
        OrderStatus.Disputed disputed => disputed.refundedCents,
        OrderStatus.Refunded => amount,
        OrderStatus.Pending or OrderStatus.Reserved or OrderStatus.Paid or OrderStatus.Failed or OrderStatus.Abandoned or OrderStatus.Canceled => 0,
    }};
    public bool settled() => match (this) {{
        OrderStatus.Refunded => true,
        OrderStatus.Pending or OrderStatus.Reserved or OrderStatus.Paid or OrderStatus.PartiallyRefunded or OrderStatus.Disputed or OrderStatus.Failed or OrderStatus.Abandoned or OrderStatus.Canceled => false,
    }};
    public Receipt snapshot() => match (this) {{
        OrderStatus.Pending pending => pending.payment,
        OrderStatus.Reserved reserved => reserved.payment,
        OrderStatus.Paid paid => paid.payment,
        OrderStatus.PartiallyRefunded partly => partly.payment,
        OrderStatus.Disputed disputed => disputed.payment,
        OrderStatus.Failed failed => failed.payment,
        OrderStatus.Abandoned abandoned => abandoned.payment,
        OrderStatus.Canceled canceled => canceled.payment,
        OrderStatus.Refunded refunded => refunded.payment,
    }};

    law aPartlyRefundedOrderHoldsARefund(OrderStatus.PartiallyRefunded partly) => partly.refundedCents > 0;

{laws}}}
"#
    )
}

/// Spec sections 20.2 and 28: a law over one case of a machine and another value holds for each value of that case
/// that keeps the case's invariants, so a law true of `Refunded` alone proves, and so does one whose claim needs the
/// invariant of `PartiallyRefunded`.
#[test]
fn a_law_over_one_case_of_a_machine_beside_another_parameter_holds_for_each_value_of_that_case() {
    let laws = "    law aFurtherRefundKeepsThePayment(OrderStatus.PartiallyRefunded partly, int cents, int amount) when cents > 0 && partly.refundedCents + cents <= amount\n        => partly.refund(cents, amount).snapshot().transactionId == partly.payment.transactionId;\n\n    law aRefundedOrderRefundsTheAmount(OrderStatus.Refunded refunded, int amount) => refunded.refunded(amount) == amount;\n";
    let order = refunding_order(laws);
    let directory = library_project(&[
        ("app/Store/OrderStatus.sharp", &order),
        ("app/Store/Receipt.sharp", RECEIPT),
        ("app/Store/Variant.sharp", VARIANT),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Store/OrderStatus.sharp").exists());
}

/// Spec sections 20.2 and 28: a law in a file of its own over one case of a machine another file declares holds for each
/// value of that case, as a law of the machine does.
#[test]
fn a_law_over_one_case_of_a_machine_another_file_declares_holds_for_each_value_of_that_case() {
    let refunds = "namespace App.Store;\n\npublic final class Refunds\n{\n    law aRefundedOrderRefundsTheAmount(OrderStatus.Refunded refunded, int amount) => refunded.refunded(amount) == amount;\n}\n";
    let order = refunding_order("");
    let directory = library_project(&[
        ("app/Store/OrderStatus.sharp", &order),
        ("app/Store/Receipt.sharp", RECEIPT),
        ("app/Store/Refunds.sharp", refunds),
        ("app/Store/Variant.sharp", VARIANT),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Store/Refunds.sharp").exists());
}

/// Spec sections 20.2 and 28: a law over two cases of a machine holds for each pair of their values that keep their
/// cases' invariants.
#[test]
fn a_law_over_two_cases_of_a_machine_holds_for_each_pair_of_their_values() {
    let laws = "    law aRefundedOrderSettlesWhereAPartlyRefundedOneHoldsARefund(OrderStatus.Refunded refunded, OrderStatus.PartiallyRefunded partly)\n        => refunded.settled() && !partly.settled() && partly.refundedCents > 0;\n";
    let order = refunding_order(laws);
    let directory = library_project(&[
        ("app/Store/OrderStatus.sharp", &order),
        ("app/Store/Receipt.sharp", RECEIPT),
        ("app/Store/Variant.sharp", VARIANT),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Store/OrderStatus.sharp").exists());
}

/// Spec section 20.2: `Json.decode` builds a case that keeps its invariants, whatever value a move gives, so a law
/// true of the one value every move builds, `Paid(5)`, is not proven, and the small values that break it keep the
/// case's invariants: `Paid(0)` breaks `paidIsPositive`, so it names `Paid(1)`.
#[test]
fn a_law_true_after_every_move_but_false_of_a_decoded_case_is_not_proven_and_names_a_value_its_case_keeps() {
    let order = "namespace App;\n\nimport Sharp.Key;\nimport Sharp.Tag;\n\n[Tag(\"status\")]\npublic machine Order\n{\n    [Key(\"open\")] case Open\n    {\n        public Order.Paid pay() => Order.Paid(5);\n    }\n\n    [Key(\"paid\")] final case Paid(int cents);\n\n    public static Order.Open open() => Order.Open;\n\n    law paidIsPositive(Order.Paid paid) => paid.cents > 0;\n\n    law paidIsFive(Order.Paid paid, int x) => paid.cents == 5;\n}\n";
    let directory = library_project(&[("app/Order.sharp", order)]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(printed(&output).contains("error[law-unproven]: law paidIsFive is not proven\n"), "{}", printed(&output));
    assert!(printed(&output).contains(" = fails for: paid = Order.Paid(1), x = 0\n"), "{}", printed(&output));
}
