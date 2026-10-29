#![allow(clippy::expect_used, clippy::missing_panics_doc)]

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;

use mago_analyzer::external::ExternalAnalyzer;
use mago_analyzer::external::ExternalAnalyzerHandle;
use mago_analyzer::plugin::PluginRegistry;
use mago_analyzer::settings::Settings as AnalyzerSettings;
use mago_database::Database;
use mago_database::DatabaseConfiguration;
use mago_database::DatabaseReader;
use mago_database::file::File;
use mago_database::file::FileId;
use mago_database::file::FileType;
use mago_database::membership::WorkspaceMatcher;
use mago_extension::WorkerCommand;
use mago_extension::WorkerPool;
use mago_extension::WorkerPoolOptions;
use mago_orchestrator::service::incremental_analysis::compile::Compilation;
use mago_php_version::PHPVersion;
use mago_reporting::IssueCollection;
use mago_server::Server;
use mago_server::Settings;
use mago_sharp_bridge::unit::Input;
use mago_sharp_bridge::unit::Library;
use mago_sharp_bridge::unit::header;
use mago_sharp_bridge::unit::source_hash;
use mago_syntax::settings::ParserSettings;

mod common;

const USER: &str = "<?php\nfunction use_box(Box $box): int { return $box->value(); }\n";

const BOX: &str = "<?php\nfinal class Box { public function value(): int { return 1; } }\n";

fn database(files: &[(&'static str, &str)]) -> Database<'static> {
    let mut database = Database::new(common::database_configuration("/server-proof", vec![Cow::Borrowed(b"src")]));
    for (name, contents) in files {
        database.add(File::new(
            Cow::Borrowed(name.as_bytes()),
            FileType::Host,
            None,
            Cow::Owned(contents.as_bytes().to_vec()),
        ));
    }

    database
}

fn server(repository: &Path, database: Database<'static>) -> Server {
    server_with(repository, database, AnalyzerSettings::new(PHPVersion::PHP85))
}

fn server_with(repository: &Path, database: Database<'static>, analyzer_settings: AnalyzerSettings) -> Server {
    let command = WorkerCommand::new("php")
        .with_argument(repository.join("composer/tests/Sdk/Fixtures/analyzer-server-worker.php"))
        .with_current_directory(repository);
    let pool = WorkerPool::spawn(command, NonZeroUsize::MIN, WorkerPoolOptions::default())
        .expect("server proof worker pool should start");
    let analyzer = ExternalAnalyzer::initialize([Arc::new(pool)], PHPVersion::PHP85, &[], false)
        .expect("server proof analyzer should initialize");
    let mut registry = PluginRegistry::with_library_providers();
    registry.set_external_analyzer(Arc::new(ExternalAnalyzerHandle::ready(analyzer)));

    let settings = Settings {
        parser: ParserSettings::default(),
        analyzer: analyzer_settings,
        plugin_registry: Arc::new(registry),
        use_progress_bars: false,
    };

    Server::new(database, Default::default, settings)
}

fn codes(issues: &IssueCollection) -> BTreeMap<String, Vec<String>> {
    let mut codes = BTreeMap::<String, Vec<String>>::new();
    for issue in issues.iter() {
        codes.entry(issue.code.clone().unwrap_or_default()).or_default().push(issue.message.clone());
    }
    codes
}

#[test]
fn a_scan_only_server_reports_only_the_before_analysis_issues() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the scan-only server test") {
        return;
    }

    let files = [
        ("src/marker.php", "<?php\nconst PROOF_BEFORE = 1;\n"),
        ("src/broken.php", "<?php\nfunction broken(): int { return 'text'; }\n"),
    ];

    let full = codes(&server(repository, database(&files)).analyze().expect("full analysis").issues);
    assert!(full.contains_key("invalid-return-statement"), "a full analysis analyzes files: {full:#?}");

    let scanned = codes(&server(repository, database(&files)).scan_only().analyze().expect("scan").issues);
    assert_eq!(scanned.keys().collect::<Vec<_>>(), ["server-proof/before"], "{scanned:#?}");
}

#[test]
fn a_failed_analysis_resets_the_server_so_the_next_answers_like_a_fresh_one() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the server reset test") {
        return;
    }

    let mut warm = server(
        repository,
        database(&[("src/user.php", USER), ("src/box.php", BOX), ("src/flag.php", "<?php\nconst PROOF_OK = 1;\n")]),
    );
    warm.analyze().expect("initial analysis");

    let flag = FileId::new(b"src/flag.php");
    warm.database_mut().update(flag, Cow::Borrowed(b"<?php\nconst PROOF_FAIL = 1;\n"));
    assert!(warm.analyze_incremental(&[flag]).is_err(), "the after-analysis hook fails the pass");

    let user = FileId::new(b"src/user.php");
    let edited = "<?php\nfunction use_box(Box $box): int { return $box->value() + 1; }\n";
    warm.database_mut().update(flag, Cow::Borrowed(b"<?php\nconst PROOF_OK = 1;\n"));
    warm.database_mut().update(user, Cow::Borrowed(edited.as_bytes()));
    let recovered = codes(&warm.analyze_incremental(&[flag, user]).expect("recovered analysis").issues);

    let fresh = codes(
        &server(
            repository,
            database(&[
                ("src/user.php", edited),
                ("src/box.php", BOX),
                ("src/flag.php", "<?php\nconst PROOF_OK = 1;\n"),
            ]),
        )
        .analyze()
        .expect("fresh analysis")
        .issues,
    );
    assert_eq!(recovered, fresh);
}

/// Declaring `PROOF_REFERENCE` gives the before-analysis hook a new reference, so the warm pass analyzes every file
/// again, the unchanged `Broken` too. Under `diff`, as `mago analyze --watch` analyzes, `Broken` keeps its issue.
#[test]
fn a_warm_pass_whose_before_analysis_references_changed_reports_what_a_fresh_pass_reports() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the changed references test") {
        return;
    }

    let broken = "<?php\nfinal class Broken { public function value(): int { return 'text'; } }\n";
    let watch = || AnalyzerSettings { diff: true, ..AnalyzerSettings::new(PHPVersion::PHP85) };
    let mut warm = server_with(
        repository,
        database(&[("src/box.php", BOX), ("src/broken.php", broken), ("src/flag.php", "<?php\nconst PROOF_OK = 1;\n")]),
        watch(),
    );
    warm.analyze().expect("initial analysis");

    let flag = FileId::new(b"src/flag.php");
    let referencing = "<?php\nconst PROOF_REFERENCE = 1;\n";
    warm.database_mut().update(flag, Cow::Borrowed(referencing.as_bytes()));
    let after = codes(&warm.analyze_incremental(&[flag]).expect("analysis after the references changed").issues);

    let fresh = codes(
        &server_with(
            repository,
            database(&[("src/box.php", BOX), ("src/broken.php", broken), ("src/flag.php", referencing)]),
            watch(),
        )
        .analyze()
        .expect("fresh analysis")
        .issues,
    );
    assert!(fresh.contains_key("invalid-return-statement"), "Broken returns a string: {fresh:#?}");
    assert_eq!(after, fresh);
}

/// The messages the reading hook reports under `code`.
fn reads(issues: &IssueCollection, code: &str) -> Vec<String> {
    let code = format!("server-reads/{code}");
    issues
        .iter()
        .filter(|issue| issue.code.as_deref() == Some(code.as_str()))
        .map(|issue| issue.message.clone())
        .collect()
}

#[test]
fn a_hook_that_read_a_class_runs_again_when_the_class_signature_changes_and_not_after_a_body_edit() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the recorded reads test") {
        return;
    }

    let mut server =
        server(repository, database(&[("src/reader.php", "<?php\n// reads: Box::value\n"), ("src/box.php", BOX)]));
    let first = server.analyze().expect("initial analysis").issues;
    assert_eq!(reads(&first, "read"), ["Run 1: `Box::value` is Public."]);

    let box_file = FileId::new(b"src/box.php");
    server
        .database_mut()
        .update(box_file, Cow::Borrowed(b"<?php\nfinal class Box { public function value(): int { return 2; } }\n"));
    let body = server.analyze_incremental(&[box_file]).expect("analysis after a body edit").issues;
    assert_eq!(reads(&body, "read"), ["Run 1: `Box::value` is Public."], "a body edit leaves the read alone");

    server
        .database_mut()
        .update(box_file, Cow::Borrowed(b"<?php\nfinal class Box { private function value(): int { return 2; } }\n"));
    let hidden = server.analyze_incremental(&[box_file]).expect("analysis after a visibility change").issues;
    assert_eq!(reads(&hidden, "read"), ["Run 2: `Box::value` is Private."]);
}

/// A PHP# parameter whose function type states no effects carries the codex's last flag, bit 63, which a hook reads
/// with every other flag of the method.
#[test]
fn a_hook_reads_a_method_that_takes_a_function_with_every_flag() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the function parameter reads test") {
        return;
    }

    let calls =
        "namespace App;\n\npublic class Calls\n{\n    public static int apply(Function<int(int)> f) => f(1);\n}\n";
    let mut server = server(
        repository,
        database(&[("src/reader.php", "<?php\n// reads: App\\Calls::apply\n"), ("src/Calls.sharp", calls)]),
    );
    let issues = server.analyze().expect("the analysis reads the method").issues;
    assert_eq!(reads(&issues, "read"), ["Run 1: `App\\Calls::apply` is Public. `$f` takes on effects."]);
}

#[test]
fn a_hook_that_listed_classes_sees_a_new_class_on_the_next_run_and_does_not_run_after_a_body_edit() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the listing reads test") {
        return;
    }

    let mut server =
        server(repository, database(&[("src/lister.php", "<?php\n// lists: classes\n"), ("src/box.php", BOX)]));
    let first = server.analyze().expect("initial analysis").issues;
    assert_eq!(reads(&first, "listing"), ["Run 1: classes Box."]);

    let box_file = FileId::new(b"src/box.php");
    server
        .database_mut()
        .update(box_file, Cow::Borrowed(b"<?php\nfinal class Box { public function value(): int { return 2; } }\n"));
    let body = server.analyze_incremental(&[box_file]).expect("analysis after a body edit").issues;
    assert_eq!(reads(&body, "listing"), ["Run 1: classes Box."], "a body edit leaves the listing alone");

    let crate_file = FileId::new(b"src/crate.php");
    server.database_mut().add(File::new(
        Cow::Borrowed(b"src/crate.php"),
        FileType::Host,
        None,
        Cow::Borrowed(b"<?php\nfinal class Crate {}\n"),
    ));
    let added = server.analyze_incremental(&[crate_file]).expect("analysis after a new class").issues;
    assert_eq!(reads(&added, "listing"), ["Run 2: classes Box, Crate."]);

    server.database_mut().delete(crate_file);
    let removed = server.analyze_incremental(&[crate_file]).expect("analysis after a removed class").issues;
    assert_eq!(reads(&removed, "listing"), ["Run 3: classes Box."]);
}

/// The messages reported under `code`, sorted, since files analyzed together report in any order.
fn reported(issues: &IssueCollection, code: &str) -> Vec<String> {
    let mut messages: Vec<String> =
        issues.iter().filter(|issue| issue.code.as_deref() == Some(code)).map(|issue| issue.message.clone()).collect();
    messages.sort();
    messages
}

/// The messages the owners hook reports.
fn owners(issues: &IssueCollection) -> Vec<String> {
    reported(issues, "server-owners/owners")
}

/// The messages the parents hook reports.
fn parents(issues: &IssueCollection) -> Vec<String> {
    reported(issues, "server-parents/parents")
}

/// `Shipment`, extending `parent`, with `total` returning `returned`.
fn shipment(parent: &str, returned: &str) -> String {
    format!(
        "<?php\nclass Base {{}}\nclass Other {{}}\nfinal class Shipment extends {parent}\n{{\n    public function total(): int {{ return {returned}; }}\n}}\n"
    )
}

/// The prepared hook reads `Shipment`'s parents before the analysis and answers both marked files from them, reading
/// nothing for either. Each file is analyzed again when `Shipment`'s parent changes, and neither after an edit of its
/// body.
#[test]
fn every_file_a_before_analysis_read_answered_runs_again_when_the_class_changes_parent_and_none_after_a_body_edit() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the before-analysis reads test") {
        return;
    }

    let mut server = server(
        repository,
        database(&[
            ("src/shipment.php", &shipment("Base", "1")),
            ("src/billing.php", "<?php\n// prepared: Base\nfinal class Billing {}\n"),
            ("src/shipping.php", "<?php\n// prepared: Base\nfinal class Shipping {}\n"),
        ]),
    );
    let prepared = |issues: &IssueCollection| reported(issues, "server-prepared/prepared");
    let first = server.analyze().expect("initial analysis").issues;
    assert_eq!(
        prepared(&first),
        [
            "Run 1 of `src/billing.php`: `Shipment` extends `Base`.",
            "Run 1 of `src/shipping.php`: `Shipment` extends `Base`."
        ]
    );

    let shipment_file = FileId::new(b"src/shipment.php");
    server.database_mut().update(shipment_file, Cow::Owned(shipment("Base", "2").into_bytes()));
    let edited = server.analyze_incremental(&[shipment_file]).expect("analysis after a body edit").issues;
    assert_eq!(prepared(&edited), prepared(&first), "a body edit runs neither again");

    server.database_mut().update(shipment_file, Cow::Owned(shipment("Other", "2").into_bytes()));
    let moved = server.analyze_incremental(&[shipment_file]).expect("analysis after a parent change").issues;
    assert_eq!(
        prepared(&moved),
        [
            "Run 2 of `src/billing.php`: `Shipment` does not extend `Base`.",
            "Run 2 of `src/shipping.php`: `Shipment` does not extend `Base`."
        ]
    );
}

/// The attribute the owners hook lists the methods of.
const OWNER: &str = "<?php\n#[Attribute]\nfinal class Owner {}\n";

/// `Order` has an `#[Owner]` method, and `Invoice` has `invoice` before its `account`.
fn models(invoice: &str) -> String {
    format!(
        "<?php\nfinal class Order\n{{\n    #[Owner]\n    public function account(): int {{ return 1; }}\n}}\nfinal class Invoice\n{{\n{invoice}    public function account(): int {{ return 1; }}\n}}\n"
    )
}

/// The hook reads the listing in whichever of the two marked files the worker analyzes first, and answers the other
/// from the listing it kept. Each file is analyzed again when the listing changes, and neither when an edit leaves it
/// as it was.
#[test]
fn every_file_a_kept_listing_answered_runs_again_when_the_listing_changes_and_none_when_it_stays() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the kept listing reads test") {
        return;
    }

    let mut server = server(
        repository,
        database(&[
            ("src/owner.php", OWNER),
            ("src/models.php", &models("")),
            ("src/orders.php", "<?php\n// owners: Order\nfinal class OrderReport {}\n"),
            ("src/invoices.php", "<?php\n// owners: Invoice\nfinal class InvoiceReport {}\n"),
        ]),
    );
    let first = server.analyze().expect("initial analysis").issues;
    assert_eq!(
        owners(&first),
        ["Run 1 of `src/invoices.php`: `Invoice` is not owned.", "Run 1 of `src/orders.php`: `Order` is owned."]
    );

    let models_file = FileId::new(b"src/models.php");
    let total = "    public function total(): int { return 2; }\n";
    server.database_mut().update(models_file, Cow::Owned(models(total).into_bytes()));
    let same = server.analyze_incremental(&[models_file]).expect("analysis after a method the listing omits").issues;
    assert_eq!(owners(&same), owners(&first), "a method with no `#[Owner]` leaves the listing as it was");

    server.database_mut().update(models_file, Cow::Owned(models("    #[Owner]\n").into_bytes()));
    let owned = server.analyze_incremental(&[models_file]).expect("analysis after a method joins the listing").issues;
    assert_eq!(
        owners(&owned),
        ["Run 2 of `src/invoices.php`: `Invoice` is owned.", "Run 2 of `src/orders.php`: `Order` is owned."]
    );
}

/// The owners hook's answers, each as `` `file`: `Class` is owned. ``, without the run that gave it.
fn owner_answers(issues: &IssueCollection) -> Vec<String> {
    owners(issues).into_iter().map(|message| message.split_once(" of ").expect("a run").1.to_owned()).collect()
}

/// The owners files, `Order` and `Invoice` each with an `#[Owner]` method when marked so.
fn owned_models(order: bool, invoice: bool) -> String {
    let owner = |owned: bool| if owned { "    #[Owner]\n" } else { "" };
    format!(
        "<?php\nfinal class Order\n{{\n{}    public function account(): int {{ return 1; }}\n}}\nfinal class Invoice\n{{\n{}    public function account(): int {{ return 1; }}\n}}\n",
        owner(order),
        owner(invoice),
    )
}

/// The owners hook reads the listing in one marked file and answers the other from it, so only the reading file's
/// state holds the read. `lose` makes that file stop reading it. A later change to the listing then reaches the other
/// file, which reports what a cold run reports.
fn a_kept_listing_reaches_the_other_file_after_its_reader(lose: fn(&mut Server, FileId)) {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the lost listing reads test") {
        return;
    }

    let marked = [
        ("src/orders.php", "<?php\n// owners: Order\nfinal class OrderReport {}\n", "Order"),
        ("src/invoices.php", "<?php\n// owners: Invoice\nfinal class InvoiceReport {}\n", "Invoice"),
    ];
    let mut warm = server(
        repository,
        database(&[
            ("src/owner.php", OWNER),
            ("src/models.php", &owned_models(true, false)),
            (marked[0].0, marked[0].1),
            (marked[1].0, marked[1].1),
        ]),
    );
    let first = warm.analyze().expect("initial analysis").issues;
    let [read] = reported(&first, "server-owners/owners-read").try_into().expect("one file read the owners");
    let reader = read.split('`').nth(1).expect("the reading file").to_owned();
    let (other, other_contents, other_class) =
        marked.into_iter().find(|(name, _, _)| *name != reader).expect("the file answered from the kept listing");

    let reader_file = FileId::new(reader.as_bytes());
    lose(&mut warm, reader_file);
    warm.analyze_incremental(&[reader_file]).expect("analysis after the reader stops reading");

    let models_file = FileId::new(b"src/models.php");
    let flipped = if other_class == "Order" { owned_models(false, false) } else { owned_models(true, true) };
    warm.database_mut().update(models_file, Cow::Owned(flipped.clone().into_bytes()));
    let changed = warm.analyze_incremental(&[models_file]).expect("analysis after the listing changes").issues;

    let cold = server(
        repository,
        database(&[("src/owner.php", OWNER), ("src/models.php", &flipped), (other, other_contents)]),
    )
    .analyze()
    .expect("cold analysis")
    .issues;
    let owned = if other_class == "Order" { "is not" } else { "is" };
    assert_eq!(owner_answers(&cold), [format!("`{other}`: `{other_class}` {owned} owned.")]);
    assert_eq!(owner_answers(&changed), owner_answers(&cold), "{reader} read the listing first");
}

#[test]
fn a_kept_listing_reaches_the_other_file_after_its_reader_stops_reading_it() {
    a_kept_listing_reaches_the_other_file_after_its_reader(|server, reader| {
        let contents = server.database().get(&reader).expect("the reading file").contents.clone();
        let unmarked = String::from_utf8_lossy(&contents)
            .lines()
            .filter(|line| !line.contains("owners: "))
            .collect::<Vec<_>>()
            .join("\n");
        server.database_mut().update(reader, Cow::Owned(unmarked.into_bytes()));
    });
}

#[test]
fn a_kept_listing_reaches_the_other_file_after_its_reader_is_deleted() {
    a_kept_listing_reaches_the_other_file_after_its_reader(|server, reader| {
        server.database_mut().delete(reader);
    });
}

/// `Order`, extending `parent`, with `total` returning `returned`.
fn order_of(parent: &str, returned: &str) -> String {
    format!(
        "<?php\nclass Base {{}}\nclass Other {{}}\nfinal class Order extends {parent}\n{{\n    public function total(): int {{ return {returned}; }}\n}}\n"
    )
}

/// The hook reads `Order`'s parents in whichever of the two marked files the worker analyzes first, and answers the
/// other from the parents it kept. Each file is analyzed again when `Order`'s parent changes, and neither after an edit
/// of `Order`'s body.
#[test]
fn every_file_a_kept_class_answered_runs_again_when_the_class_changes_parent_and_none_after_a_body_edit() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the kept class reads test") {
        return;
    }

    let mut server = server(
        repository,
        database(&[
            ("src/order.php", &order_of("Base", "1")),
            ("src/billing.php", "<?php\n// extends: Order Base\nfinal class Billing {}\n"),
            ("src/shipping.php", "<?php\n// extends: Order Base\nfinal class Shipping {}\n"),
        ]),
    );
    let first = server.analyze().expect("initial analysis").issues;
    assert_eq!(
        parents(&first),
        ["Run 1 of `src/billing.php`: `Order` extends `Base`.", "Run 1 of `src/shipping.php`: `Order` extends `Base`."]
    );

    let order_file = FileId::new(b"src/order.php");
    server.database_mut().update(order_file, Cow::Owned(order_of("Base", "2").into_bytes()));
    let edited = server.analyze_incremental(&[order_file]).expect("analysis after a body edit").issues;
    assert_eq!(parents(&edited), parents(&first), "a body edit runs neither again");

    server.database_mut().update(order_file, Cow::Owned(order_of("Other", "2").into_bytes()));
    let moved = server.analyze_incremental(&[order_file]).expect("analysis after a parent change").issues;
    assert_eq!(
        parents(&moved),
        [
            "Run 2 of `src/billing.php`: `Order` does not extend `Base`.",
            "Run 2 of `src/shipping.php`: `Order` does not extend `Base`."
        ]
    );
}

/// The messages the reading hook reports under `comparison`, sorted, since files analyzed together report in any order.
fn comparisons(issues: &IssueCollection) -> Vec<String> {
    let mut messages = reads(issues, "comparison");
    messages.sort();
    messages
}

/// Two files whose hook compares `Box`: the worker answers the second file's comparison from its cache.
#[test]
fn hooks_that_compared_a_class_run_again_when_the_class_changes_parent_and_not_after_a_body_edit() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the comparison reads test") {
        return;
    }

    let mut server = server(
        repository,
        database(&[
            ("src/billing.php", "<?php\n// compares: Box\n"),
            ("src/payroll.php", "<?php\n// compares: Box\n"),
            ("src/base.php", "<?php\nclass Base {}\n"),
            ("src/box.php", BOX),
        ]),
    );
    let first = server.analyze().expect("initial analysis").issues;
    assert_eq!(comparisons(&first), ["Run 1: `Box` is not a `Base`.", "Run 2: `Box` is not a `Base`."]);

    let box_file = FileId::new(b"src/box.php");
    server
        .database_mut()
        .update(box_file, Cow::Borrowed(b"<?php\nfinal class Box { public function value(): int { return 2; } }\n"));
    let body = server.analyze_incremental(&[box_file]).expect("analysis after a body edit").issues;
    assert_eq!(comparisons(&body), comparisons(&first), "a body edit leaves the comparisons alone");

    server.database_mut().update(
        box_file,
        Cow::Borrowed(b"<?php\nfinal class Box extends Base { public function value(): int { return 2; } }\n"),
    );
    let extended = server.analyze_incremental(&[box_file]).expect("analysis after a parent change").issues;
    assert_eq!(comparisons(&extended), ["Run 3: `Box` is a `Base`.", "Run 4: `Box` is a `Base`."]);
}

/// The model whose `total` return type the fixture's provider gives `Query::total`.
fn order(returned: &str) -> String {
    format!(
        "<?php\n\nnamespace App\\Models;\n\nfinal class Order\n{{\n    public int $count = 0;\n\n    public function total(): {returned}\n    {{\n        return {};\n    }}\n}}\n",
        if returned == "int" { "1" } else { "'1'" }
    )
}

const QUERY: &str = "<?php\n\nnamespace App\\Models;\n\nfinal class Query\n{\n    public function total(): mixed\n    {\n        return null;\n    }\n\n    public function models(): mixed\n    {\n        return null;\n    }\n\n    public function billable(): mixed\n    {\n        return null;\n    }\n}\n";

/// A PHP# file whose `int` return holds only while the provider answers `Query::total` with `int`.
const REVENUE: &str = "namespace App;\n\nimport App.Models.Query;\n\npublic class Revenue\n{\n    public int total(Query query)\n    {\n        return query.total();\n    }\n}\n";

fn shop(order: &str) -> Database<'static> {
    database(&[("app/Models/Order.php", order), ("app/Models/Query.php", QUERY), ("app/Revenue.sharp", REVENUE)])
}

#[test]
fn a_file_whose_type_a_provider_read_from_a_class_is_analyzed_again_when_the_class_signature_changes() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the provider reads test") {
        return;
    }

    let mut warm = server(repository, shop(&order("int")));
    let first = codes(&warm.analyze().expect("initial analysis").issues);
    assert!(!first.contains_key("invalid-return-statement"), "the provider answers int: {first:#?}");

    let order_file = FileId::new(b"app/Models/Order.php");
    let edited = order("string");
    warm.database_mut().update(order_file, Cow::Owned(edited.clone().into_bytes()));
    let after = codes(&warm.analyze_incremental(&[order_file]).expect("analysis after a signature change").issues);

    let fresh = codes(&server(repository, shop(&edited)).analyze().expect("fresh analysis").issues);
    assert!(fresh.contains_key("invalid-return-statement"), "the provider answers string: {fresh:#?}");
    assert_eq!(after, fresh);
}

/// Each input `server` names for its one compiled PHP# file, stamped from the database, and the file's bytes.
fn compiled(server: &mut Server) -> (BTreeMap<String, Option<Input>>, Vec<u8>) {
    let database = server.database().read_only();
    let mut inputs = BTreeMap::new();
    let compiled = server
        .compile(
            |path| {
                let input = database.get(&FileId::new(path)).ok().map(|file| Input {
                    path: path.to_vec(),
                    size: file.contents.len() as u64,
                    mtime_ns: 0,
                    hash: source_hash(&file.contents),
                });
                inputs.insert(String::from_utf8_lossy(path).into_owned(), input.clone());
                Ok(input)
            },
            |_| Ok(Default::default()),
            &Library::default(),
            |issues| issues,
        )
        .expect("the compile runs");
    let [(_, Compilation::Accepted(bytes))] = compiled.as_slice() else {
        panic!("the PHP# file is not accepted: {compiled:?}");
    };

    (inputs, bytes.clone())
}

#[test]
fn a_sharp_file_whose_type_a_provider_read_from_a_model_lists_the_model_among_its_inputs() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the provider reads compile test") {
        return;
    }

    let mut server = server(repository, shop(&order("int")));
    server.analyze().expect("initial analysis");
    let (before, before_bytes) = compiled(&mut server);
    assert!(before.contains_key("app/Models/Order.php"), "the model the provider read: {:?}", before.keys());

    let order_file = FileId::new(b"app/Models/Order.php");
    let edited = order("int").replace("return 1;", "return 2;");
    server.database_mut().update(order_file, Cow::Owned(edited.into_bytes()));
    server.analyze_incremental(&[order_file]).expect("analysis after a body edit");
    let (after, after_bytes) = compiled(&mut server);

    assert_ne!(before["app/Models/Order.php"], after["app/Models/Order.php"]);
    assert_eq!(header(&before_bytes).expect("a header").key, header(&after_bytes).expect("a header").key);
    assert_ne!(before_bytes, after_bytes, "the compiled file holds the model's new stamp");
}

#[test]
fn changing_a_property_type_of_the_model_a_provider_read_gives_the_sharp_file_a_new_key() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the provider reads key test") {
        return;
    }

    let mut server = server(repository, shop(&order("int")));
    server.analyze().expect("initial analysis");
    let (_, before) = compiled(&mut server);

    let order_file = FileId::new(b"app/Models/Order.php");
    let edited = order("int").replace("public int $count = 0;", "public string $count = '';");
    server.database_mut().update(order_file, Cow::Owned(edited.into_bytes()));
    server.analyze_incremental(&[order_file]).expect("analysis after a property type change");
    let (_, after) = compiled(&mut server);

    assert_ne!(header(&before).expect("a header").key, header(&after).expect("a header").key);
}

#[test]
fn changing_a_property_type_of_the_parent_of_the_model_a_provider_read_gives_the_sharp_file_a_new_key() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the provider reads parent key test") {
        return;
    }

    let parent = "<?php\n\nnamespace App\\Models;\n\nclass Model\n{\n    public int $count = 0;\n}\n";
    let order = order("int").replace("final class Order\n", "final class Order extends Model\n");
    let order = order.replace("    public int $count = 0;\n\n", "");
    let mut server = server(
        repository,
        database(&[
            ("app/Models/Model.php", parent),
            ("app/Models/Order.php", &order),
            ("app/Models/Query.php", QUERY),
            ("app/Revenue.sharp", REVENUE),
        ]),
    );
    server.analyze().expect("initial analysis");
    let (_, before) = compiled(&mut server);

    let parent_file = FileId::new(b"app/Models/Model.php");
    let edited = parent.replace("public int $count = 0;", "public string $count = '';");
    server.database_mut().update(parent_file, Cow::Owned(edited.into_bytes()));
    server.analyze_incremental(&[parent_file]).expect("analysis after a parent's property type change");
    let (_, after) = compiled(&mut server);

    assert_ne!(header(&before).expect("a header").key, header(&after).expect("a header").key);
}

/// A PHP# file whose `int` return holds only while the provider, which lists every class to answer, answers
/// `Query::models` with `int`.
const CATALOG: &str = "namespace App;\n\nimport App.Models.Query;\n\npublic class Catalog\n{\n    public int models(Query query)\n    {\n        return query.models();\n    }\n}\n";

#[test]
fn adding_a_class_to_the_classes_a_provider_listed_gives_the_sharp_file_a_new_key() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the provider listing key test") {
        return;
    }

    let mut server = server(repository, database(&[("app/Models/Query.php", QUERY), ("app/Catalog.sharp", CATALOG)]));
    server.analyze().expect("initial analysis");
    let (_, before) = compiled(&mut server);

    let invoice = server.database_mut().add(File::new(
        Cow::Borrowed(b"app/Models/Invoice.php"),
        FileType::Host,
        None,
        Cow::Borrowed(b"<?php\n\nnamespace App\\Models;\n\nfinal class Invoice {}\n"),
    ));
    server.analyze_incremental(&[invoice]).expect("analysis after a new class");
    let (_, after) = compiled(&mut server);

    assert_ne!(header(&before).expect("a header").key, header(&after).expect("a header").key);
}

/// The model the fixture's provider compares against `App\Models\Model`, declared with `parent` as its parent class.
fn order_extending(parent: &str) -> String {
    format!("<?php\n\nnamespace App\\Models;\n\nfinal class Order extends {parent} {{}}\n")
}

/// A PHP# file whose `int` return the provider answers by asking whether `Order` is a `Model`.
const BILLING: &str = "namespace App;\n\nimport App.Models.Query;\n\npublic class Billing\n{\n    public int billable(Query query)\n    {\n        return query.billable();\n    }\n}\n";

#[test]
fn changing_the_parent_of_a_model_a_provider_compared_changes_the_sharp_file_inputs_and_key() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the comparator reads compile test") {
        return;
    }

    let mut server = server(
        repository,
        database(&[
            ("app/Models/Model.php", "<?php\n\nnamespace App\\Models;\n\nclass Model {}\n"),
            ("app/Models/Record.php", "<?php\n\nnamespace App\\Models;\n\nclass Record {}\n"),
            ("app/Models/Order.php", &order_extending("Model")),
            ("app/Models/Query.php", QUERY),
            ("app/Billing.sharp", BILLING),
        ]),
    );
    server.analyze().expect("initial analysis");
    let (before, before_bytes) = compiled(&mut server);
    assert!(before.contains_key("app/Models/Order.php"), "the model the provider compared: {:?}", before.keys());

    let order_file = FileId::new(b"app/Models/Order.php");
    server.database_mut().update(order_file, Cow::Owned(order_extending("Record").into_bytes()));
    server.analyze_incremental(&[order_file]).expect("analysis after a parent change");
    let (after, after_bytes) = compiled(&mut server);

    assert_ne!(before["app/Models/Order.php"], after["app/Models/Order.php"]);
    assert_ne!(header(&before_bytes).expect("a header").key, header(&after_bytes).expect("a header").key);
}

/// `Line`, with an `#[Relation]` method for each of `relations`.
fn lines(relations: &[&str]) -> String {
    let methods: String = relations
        .iter()
        .map(|relation| format!("    #[\\Relation]\n    public function {relation}(): int {{ return 1; }}\n"))
        .collect();
    format!("<?php\n\nnamespace App\\Models;\n\nfinal class Line\n{{\n{methods}}}\n")
}

/// A PHP# file whose `int` return holds only while the relations plugin's provider answers `Order::$items` with `int`.
const ORDERS: &str = "namespace App;\n\nimport App.Models.Order;\n\npublic class Orders\n{\n    public int count(Order order)\n    {\n        return order.items;\n    }\n}\n";

/// The relations plugin searches the methods before the analysis, and its provider answers `Order::$items` from what
/// the search found, reading nothing itself. The search can change the PHP# file's code, so the file declaring a
/// relation is among its inputs, and a relation that joins gives the file a new key.
#[test]
fn a_sharp_file_whose_type_a_provider_answered_from_its_plugins_before_analysis_read_lists_what_it_read_among_its_inputs()
 {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the provider plugin reads compile test") {
        return;
    }

    let mut server = server(
        repository,
        database(&[
            ("app/Models/Relation.php", "<?php\n\n#[Attribute]\nfinal class Relation {}\n"),
            ("app/Models/Line.php", &lines(&["order"])),
            (
                "app/Models/Order.php",
                "<?php\n\nnamespace App\\Models;\n\nfinal class Order\n{\n    public function __get(string $name): mixed\n    {\n        return 1;\n    }\n}\n",
            ),
            ("app/Orders.sharp", ORDERS),
        ]),
    );
    server.analyze().expect("initial analysis");
    let (before, before_bytes) = compiled(&mut server);
    assert!(
        before.contains_key("app/Models/Line.php"),
        "the file of the relation the plugin read: {:?}",
        before.keys()
    );

    let line_file = FileId::new(b"app/Models/Line.php");
    server.database_mut().update(line_file, Cow::Owned(lines(&["order", "product"]).into_bytes()));
    server.analyze_incremental(&[line_file]).expect("analysis after a relation joins");
    let (after, after_bytes) = compiled(&mut server);

    assert_ne!(before["app/Models/Line.php"], after["app/Models/Line.php"]);
    assert_ne!(header(&before_bytes).expect("a header").key, header(&after_bytes).expect("a header").key);
}

/// A PHP# file whose class declaration the owners hook answers from the listing it kept.
const REPORT: &str = "namespace App;\n\npublic class Report\n{\n    // owners: Invoice\n}\n";

/// The inputs and the key of `app/Report.sharp`, and the owners hook's reports, from a server whose node hooks run in
/// `app/Report.sharp` alone, as `mago compile` runs them in its PHP# files, with `invoice` before `Invoice::account`.
fn compiled_report(repository: &Path, invoice: &str) -> (Vec<String>, [u8; 16], Vec<String>) {
    let files = [
        ("app/owner.php", OWNER.to_owned()),
        ("app/models.php", models(invoice)),
        ("app/orders.php", "<?php\n// owners: Order\nfinal class OrderReport {}\n".to_owned()),
        ("app/Report.sharp", REPORT.to_owned()),
    ];
    let mut database = Database::new(common::database_configuration("/server-proof", vec![Cow::Borrowed(b"app")]));
    for (name, contents) in files {
        database.add(File::new(
            Cow::Borrowed(name.as_bytes()),
            FileType::Host,
            Some(Path::new("/server-proof").join(name)),
            Cow::Owned(contents.into_bytes()),
        ));
    }
    let scope = WorkspaceMatcher::from_configuration(&DatabaseConfiguration::new(
        Path::new("/server-proof"),
        vec![b"app/Report.sharp"],
        vec![],
        vec![],
        vec![b"sharp"],
    ))
    .expect("the scope compiles");
    let mut server = server(repository, database).scoped_to(scope);
    let reports = owners(&server.analyze().expect("the analysis runs").issues);
    let (inputs, bytes) = compiled(&mut server);

    (inputs.into_keys().collect(), header(&bytes).expect("a header").key, reports)
}

/// The owners hook answers `app/Report.sharp` from the listing it reads, which decides only the issue it reports. So
/// the listing enters the PHP# file's key, which changes when a method joins the listing and stays when an edit leaves
/// the listing as it was, and none of the files it lists from is an input: their edits never make the file stale.
#[test]
fn a_compile_keys_the_listing_its_hooks_read_and_lists_none_of_its_files_among_the_inputs() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the hook reads compile test") {
        return;
    }

    let (inputs, key, reports) = compiled_report(repository, "");
    let (same_inputs, same_key, _) = compiled_report(repository, "    public function total(): int { return 2; }\n");
    let (owned_inputs, owned_key, owned_reports) = compiled_report(repository, "    #[Owner]\n");

    assert_eq!(
        reports,
        ["Run 1 of `app/Report.sharp`: `Invoice` is not owned."],
        "the hook runs in the PHP# file alone"
    );
    assert_eq!(owned_reports, ["Run 1 of `app/Report.sharp`: `Invoice` is owned."]);
    assert_eq!(inputs, ["Module.sharp", "app/Module.sharp", "app/Report.sharp", "composer.lock"]);
    assert_eq!(same_inputs, inputs);
    assert_eq!(owned_inputs, inputs);
    assert_eq!(same_key, key, "an edit that leaves the listing as it was keeps the key");
    assert_ne!(owned_key, key, "a method that joins the listing gives a new key");
}
