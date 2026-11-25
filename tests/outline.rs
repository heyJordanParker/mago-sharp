use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;

use mago_database::process;
use serde_json::Value;
use serde_json::json;

fn repository_file(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(path)
}

/// Runs `mago outline --json --stdin-input <label> < <file>`, as a shell runs it, in an empty folder with no `mago.toml`, so a
/// run that read a project's configuration fails.
fn outline_stdin(label: &str, file: &Path) -> Output {
    let folder = tempfile::tempdir().unwrap();
    process::output(
        Command::new("sh")
            .args(["-c", r#"exec "$0" outline --json --stdin-input "$1" < "$2""#, env!("CARGO_BIN_EXE_mago"), label])
            .arg(file)
            .current_dir(folder.path()),
    )
    .unwrap()
}

fn outline_of(path: &str) -> Value {
    let output = outline_stdin(path, &repository_file(path));
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));

    serde_json::from_slice(&output.stdout).unwrap()
}

fn outline_of_source(label: &str, source: &str) -> (Output, Value) {
    let folder = tempfile::tempdir().unwrap();
    let file = folder.path().join("source");
    std::fs::write(&file, source).unwrap();
    let output = outline_stdin(label, &file);
    let outline = serde_json::from_slice(&output.stdout).unwrap();

    (output, outline)
}

/// A range as the Language Server Protocol writes it: 0-based lines, and characters in UTF-16 code units.
fn range(start_line: u32, start_character: u32, end_line: u32, end_character: u32) -> Value {
    json!({
        "start": {"line": start_line, "character": start_character},
        "end": {"line": end_line, "character": end_character}
    })
}

/// A member of a class, as the Language Server Protocol's `DocumentSymbol` and the outline's fields write it: `fields`
/// holds the ones a kind adds, as a method's `complexity` and `returnType`.
fn member(
    name: &str,
    kind: &str,
    detail: &str,
    range: Value,
    selection: Value,
    visibility: &str,
    fields: Value,
) -> Value {
    let mut member = json!({
        "name": name, "kind": kind, "detail": detail, "range": range, "selectionRange": selection,
        "visibility": visibility, "supertypes": [], "attributes": [], "children": []
    });
    member.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());

    member
}

#[test]
fn a_sharp_file_prints_its_symbols_imports_and_references() {
    let item = member(
        "TItem",
        "typeParameter",
        "TItem : DatabaseEntity",
        range(14, 30, 14, 52),
        range(14, 30, 14, 35),
        "public",
        json!({}),
    );
    let t = member("T", "typeParameter", "T", range(27, 19, 27, 20), range(27, 19, 27, 20), "public", json!({}));
    let shareable = member(
        "TItem",
        "typeParameter",
        "TItem : DatabaseEntity & Shareable",
        range(31, 24, 31, 58),
        range(31, 24, 31, 29),
        "public",
        json!({}),
    );
    let children = [
        member(
            "saved",
            "field",
            "private Map<string, DatabaseEntity> saved",
            range(7, 4, 7, 52),
            range(7, 40, 7, 45),
            "private",
            json!({"type": null}),
        ),
        member(
            "kinds",
            "field",
            "private Map<string, Class<DatabaseEntity>> kinds",
            range(9, 4, 9, 80),
            range(9, 47, 9, 52),
            "private",
            json!({"type": null}),
        ),
        member(
            "pick",
            "method",
            "public static TItem? pick<TItem : DatabaseEntity>(List<TItem> items, int id)",
            range(14, 4, 22, 5),
            range(14, 25, 14, 29),
            "public",
            json!({"complexity": 3, "returnType": "TItem", "children": [item]}),
        ),
        member(
            "first",
            "method",
            "public T first<T>(List<T> items)",
            range(27, 4, 27, 49),
            range(27, 13, 27, 18),
            "public",
            json!({"complexity": 1, "returnType": "T", "children": [t]}),
        ),
        member(
            "share",
            "method",
            "public string share<TItem : DatabaseEntity & Shareable>(TItem item)",
            range(31, 4, 31, 87),
            range(31, 18, 31, 23),
            "public",
            json!({"complexity": 1, "returnType": null, "children": [shareable]}),
        ),
        member(
            "save",
            "method",
            "public void save(Class<DatabaseEntity> type, DatabaseEntity entity)",
            range(33, 4, 33, 141),
            range(33, 16, 33, 20),
            "public",
            json!({"complexity": 1, "returnType": null}),
        ),
        member(
            "load",
            "method",
            "public DatabaseEntity? load(Class<DatabaseEntity> type, int id)",
            range(35, 4, 35, 129),
            range(35, 27, 35, 31),
            "public",
            json!({"complexity": 2, "returnType": "Paging.DatabaseEntity"}),
        ),
        member(
            "kind",
            "method",
            "public Class<DatabaseEntity> kind(string name)",
            range(37, 4, 37, 88),
            range(37, 33, 37, 37),
            "public",
            json!({"complexity": 2, "returnType": null}),
        ),
    ];
    let mut store =
        member("Store", "class", "public class Store", range(4, 0, 38, 1), range(4, 13, 4, 18), "public", json!({}));
    let references = json!([
        {"name": "Order", "kind": "type", "range": range(9, 72, 9, 77), "receiverType": null, "target": "Paging.Order"},
        {"name": "link", "kind": "call", "range": range(31, 80, 31, 84), "receiverType": null, "target": null, "receiverCall": null},
        {"name": "named", "kind": "call", "range": range(33, 92, 33, 97), "receiverType": "Lib.Probe", "target": "Lib.Probe.named", "receiverCall": null},
        {"name": "named", "kind": "call", "range": range(35, 88, 35, 93), "receiverType": "Lib.Probe", "target": "Lib.Probe.named", "receiverCall": null},
        {"name": "Order", "kind": "type", "range": range(37, 81, 37, 86), "receiverType": null, "target": "Paging.Order"}
    ]);
    store["children"] = json!(children);

    assert_eq!(
        outline_of("sharp/mago/tests/fixtures/outline/Store.sharp"),
        json!({
            "version": 1,
            "language": "sharp",
            "path": "sharp/mago/tests/fixtures/outline/Store.sharp",
            "namespace": "Paging",
            "errors": [],
            "symbols": [store],
            "imports": [{"path": "Lib.Probe", "alias": null, "range": range(2, 7, 2, 16)}],
            "references": references
        })
    );
}

/// A reference as a test writes it: its name, kind, range, receiver type, target, and the index of the call it is made
/// on.
type Shown<'a> = (&'a str, &'a str, Value, Option<&'a str>, Option<&'a str>, Option<usize>);

#[test]
fn a_struct_prints_each_member_with_its_complexity_and_type() {
    let members = [
        member(
            "Date",
            "constructor",
            "public Date(public int year { get; }, public int month { get; }, public int day { get; })",
            range(5, 4, 10, 5),
            range(5, 11, 5, 15),
            "public",
            json!({"complexity": 5, "returnType": "Sharp.Time.Date"}),
        ),
        member(
            "year",
            "property",
            "public int year",
            range(5, 16, 5, 40),
            range(5, 27, 5, 31),
            "public",
            json!({"type": null}),
        ),
        member(
            "month",
            "property",
            "public int month",
            range(5, 42, 5, 67),
            range(5, 53, 5, 58),
            "public",
            json!({"type": null}),
        ),
        member(
            "day",
            "property",
            "public int day",
            range(5, 69, 5, 92),
            range(5, 80, 5, 83),
            "public",
            json!({"type": null}),
        ),
        member(
            "ofEpochDay",
            "method",
            "public static Date ofEpochDay(int days)",
            range(13, 4, 24, 5),
            range(13, 23, 13, 33),
            "public",
            json!({"complexity": 3, "returnType": "Sharp.Time.Date"}),
        ),
        member(
            "epochDay",
            "property",
            "public int epochDay",
            range(27, 4, 27, 79),
            range(27, 15, 27, 23),
            "public",
            json!({"type": null}),
        ),
        member(
            "weekday",
            "property",
            "public Weekday weekday",
            range(30, 4, 30, 75),
            range(30, 19, 30, 26),
            "public",
            json!({"type": "Sharp.Time.Weekday"}),
        ),
        member(
            "monthOfYear",
            "property",
            "public Month monthOfYear",
            range(32, 4, 32, 55),
            range(32, 17, 32, 28),
            "public",
            json!({"type": "Sharp.Time.Month"}),
        ),
        member(
            "lengthOfMonth",
            "property",
            "public int lengthOfMonth",
            range(35, 4, 35, 72),
            range(35, 15, 35, 28),
            "public",
            json!({"type": null}),
        ),
        member(
            "plusDays",
            "method",
            "public Date plusDays(int days)",
            range(37, 4, 37, 76),
            range(37, 16, 37, 24),
            "public",
            json!({"complexity": 1, "returnType": "Sharp.Time.Date"}),
        ),
        member(
            "plusMonths",
            "method",
            "public Date plusMonths(int months)",
            range(40, 4, 47, 5),
            range(40, 16, 40, 26),
            "public",
            json!({"complexity": 1, "returnType": "Sharp.Time.Date"}),
        ),
        member(
            "daysUntil",
            "method",
            "public int daysUntil(Date later)",
            range(49, 4, 49, 71),
            range(49, 15, 49, 24),
            "public",
            json!({"complexity": 1, "returnType": null}),
        ),
        member(
            "operator <=>",
            "method",
            "public static int operator <=>(Date a, Date b)",
            range(52, 4, 52, 80),
            range(52, 22, 52, 34),
            "public",
            json!({"complexity": 3, "returnType": null}),
        ),
        member(
            "atStartOfDay",
            "method",
            "public DateTime atStartOfDay(TimeZone zone)",
            range(55, 4, 55, 102),
            range(55, 20, 55, 32),
            "public",
            json!({"complexity": 1, "returnType": "Sharp.Time.DateTime"}),
        ),
        member(
            "toString",
            "method",
            "public string toString()",
            range(58, 4, 63, 5),
            range(58, 18, 58, 26),
            "public",
            json!({"complexity": 3, "returnType": null}),
        ),
        member(
            "padded",
            "method",
            "internal static string padded(int value, int width)",
            range(67, 4, 68, 127),
            range(67, 27, 67, 33),
            "internal",
            json!({"complexity": 3, "returnType": null}),
        ),
        member(
            "tenTo",
            "method",
            "private static int tenTo(int power)",
            range(71, 4, 71, 87),
            range(71, 23, 71, 28),
            "private",
            json!({"complexity": 2, "returnType": null}),
        ),
        member(
            "parse",
            "method",
            "public static Date parse(string text)",
            range(73, 4, 74, 140),
            range(73, 23, 73, 28),
            "public",
            json!({"complexity": 2, "returnType": "Sharp.Time.Date"}),
        ),
        member(
            "tryParse",
            "method",
            "public static Date? tryParse(string text)",
            range(76, 4, 86, 5),
            range(76, 24, 76, 32),
            "public",
            json!({"complexity": 6, "returnType": "Sharp.Time.Date"}),
        ),
        member(
            "daysFromCivil",
            "method",
            "private static int daysFromCivil(int year, int month, int day)",
            range(89, 4, 97, 5),
            range(89, 23, 89, 36),
            "private",
            json!({"complexity": 3, "returnType": null}),
        ),
        member(
            "daysInMonth",
            "method",
            "private static int daysInMonth(int year, int month)",
            range(99, 4, 102, 80),
            range(99, 23, 99, 34),
            "private",
            json!({"complexity": 9, "returnType": null}),
        ),
    ];
    let mut date =
        member("Date", "struct", "public struct Date", range(3, 0, 103, 1), range(3, 14, 3, 18), "public", json!({}));
    date["children"] = json!(members);
    let references: Vec<Value> = [
        (
            "daysInMonth",
            "call",
            range(7, 61, 7, 72),
            Some("Sharp.Time.Date"),
            Some("Sharp.Time.Date.daysInMonth"),
            None,
        ),
        ("ValueError", "new", range(8, 22, 8, 32), None, Some("ValueError"), None),
        ("floorDiv", "call", range(16, 28, 16, 36), None, None, None),
        ("Date", "new", range(23, 19, 23, 23), None, Some("Sharp.Time.Date"), None),
        (
            "daysFromCivil",
            "call",
            range(27, 32, 27, 45),
            Some("Sharp.Time.Date"),
            Some("Sharp.Time.Date.daysFromCivil"),
            None,
        ),
        ("from", "call", range(30, 38, 30, 42), Some("Sharp.Time.Weekday"), Some("Sharp.Time.Weekday.from"), None),
        ("mod", "call", range(30, 63, 30, 66), None, None, None),
        ("from", "call", range(32, 38, 32, 42), Some("Sharp.Time.Month"), Some("Sharp.Time.Month.from"), None),
        (
            "daysInMonth",
            "call",
            range(35, 37, 35, 48),
            Some("Sharp.Time.Date"),
            Some("Sharp.Time.Date.daysInMonth"),
            None,
        ),
        (
            "ofEpochDay",
            "call",
            range(37, 43, 37, 53),
            Some("Sharp.Time.Date"),
            Some("Sharp.Time.Date.ofEpochDay"),
            None,
        ),
        ("floorDiv", "call", range(43, 27, 43, 35), None, None, None),
        ("mod", "call", range(44, 28, 44, 31), None, None, None),
        ("Date", "new", range(46, 19, 46, 23), None, Some("Sharp.Time.Date"), None),
        ("min", "call", range(46, 42, 46, 45), Some("Sharp.Math"), Some("Sharp.Math.min"), None),
        (
            "daysInMonth",
            "call",
            range(46, 61, 46, 72),
            Some("Sharp.Time.Date"),
            Some("Sharp.Time.Date.daysInMonth"),
            None,
        ),
        (
            "instantAt",
            "call",
            range(55, 56, 55, 65),
            Some("Sharp.Time.TimeZone"),
            Some("Sharp.Time.TimeZone.instantAt"),
            None,
        ),
        ("inZone", "call", range(55, 89, 55, 95), None, None, Some(15)),
        ("padded", "call", range(60, 48, 60, 54), Some("Sharp.Time.Date"), Some("Sharp.Time.Date.padded"), None),
        ("padded", "call", range(60, 123, 60, 129), Some("Sharp.Time.Date"), Some("Sharp.Time.Date.padded"), None),
        ("padded", "call", range(62, 33, 62, 39), Some("Sharp.Time.Date"), Some("Sharp.Time.Date.padded"), None),
        ("padded", "call", range(62, 68, 62, 74), Some("Sharp.Time.Date"), Some("Sharp.Time.Date.padded"), None),
        ("tenTo", "call", range(68, 55, 68, 60), Some("Sharp.Time.Date"), Some("Sharp.Time.Date.tenTo"), None),
        ("padded", "call", range(68, 102, 68, 108), Some("Sharp.Time.Date"), Some("Sharp.Time.Date.padded"), None),
        ("tenTo", "call", range(71, 70, 71, 75), Some("Sharp.Time.Date"), Some("Sharp.Time.Date.tenTo"), None),
        ("tryParse", "call", range(74, 16, 74, 24), Some("Sharp.Time.Date"), Some("Sharp.Time.Date.tryParse"), None),
        ("ValueError", "new", range(74, 44, 74, 54), None, Some("ValueError"), None),
        (
            "preg_match",
            "call",
            range(78, 19, 78, 29),
            Some("Sharp.Time.global"),
            Some("Sharp.Time.global.preg_match"),
            None,
        ),
        ("parse", "call", range(81, 25, 81, 30), Some("Sharp.Int"), Some("Sharp.Int.parse"), None),
        ("substr", "call", range(81, 38, 81, 44), Some("Sharp.Time.global"), Some("Sharp.Time.global.substr"), None),
        ("parse", "call", range(82, 26, 82, 31), Some("Sharp.Int"), Some("Sharp.Int.parse"), None),
        ("substr", "call", range(82, 39, 82, 45), Some("Sharp.Time.global"), Some("Sharp.Time.global.substr"), None),
        ("parse", "call", range(83, 24, 83, 29), Some("Sharp.Int"), Some("Sharp.Int.parse"), None),
        ("substr", "call", range(83, 37, 83, 43), Some("Sharp.Time.global"), Some("Sharp.Time.global.substr"), None),
        (
            "daysInMonth",
            "call",
            range(85, 68, 85, 79),
            Some("Sharp.Time.Date"),
            Some("Sharp.Time.Date.daysInMonth"),
            None,
        ),
        ("Date", "new", range(85, 99, 85, 103), None, Some("Sharp.Time.Date"), None),
        ("floorDiv", "call", range(92, 32, 92, 40), None, None, None),
    ]
    .into_iter()
    .map(|(name, kind, range, receiver_type, target, receiver_call): Shown| {
        let mut reference =
            json!({"name": name, "kind": kind, "range": range, "receiverType": receiver_type, "target": target});
        if kind == "call" {
            reference["receiverCall"] = json!(receiver_call);
        }

        reference
    })
    .collect();

    assert_eq!(
        outline_of("sharp/mago/tests/fixtures/outline/Date.sharp"),
        json!({
            "version": 1,
            "language": "sharp",
            "path": "sharp/mago/tests/fixtures/outline/Date.sharp",
            "namespace": "Sharp.Time",
            "errors": [],
            "symbols": [date],
            "imports": [],
            "references": references
        })
    );
}

#[test]
fn without_json_a_file_prints_its_symbols_as_an_indented_tree() {
    let output = process::output(
        Command::new(env!("CARGO_BIN_EXE_mago"))
            .args(["outline", "sharp/mago/tests/fixtures/outline/Store.sharp"])
            .current_dir(repository_file("")),
    )
    .unwrap();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "sharp/mago/tests/fixtures/outline/Store.sharp\n  class Store 4:0\n    field saved 7:4\n    field kinds 9:4\n    method pick 14:4\n      typeParameter TItem 14:30\n    method first 27:4\n      typeParameter T 27:19\n    method share 31:4\n      typeParameter TItem 31:24\n    method save 33:4\n    method load 35:4\n    method kind 37:4\n"
    );
}

#[test]
fn each_function_like_names_the_type_a_call_on_its_result_resolves_against() {
    let (output, outline) = outline_of_source(
        "app/Shop/Orders.sharp",
        "namespace Shop;\n\nimport App.Http.Response;\n\npublic class Orders\n{\n    public Orders() { }\n    public Response respond() => new Response();\n    public Response? find() => null;\n    public PaginatedList<Order> page() => new PaginatedList<Order>();\n    public T first<T>(List<T> items) => items[0];\n    public int|string key() => 1;\n    public Countable & Traversable both() => this;\n    public void clear() { }\n    law keepsItself(int a) => a == a;\n    public string label() => \"\";\n    public List<Order> all() => [];\n}\n",
    );
    let return_types: Vec<(&str, &Value)> = outline["symbols"][0]["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|child| (child["name"].as_str().unwrap(), &child["returnType"]))
        .collect();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(
        return_types,
        [
            ("Orders", &json!("Shop.Orders")),
            ("respond", &json!("App.Http.Response")),
            ("find", &json!("App.Http.Response")),
            ("page", &json!("Shop.PaginatedList")),
            ("first", &json!("T")),
            ("key", &Value::Null),
            ("both", &Value::Null),
            ("clear", &Value::Null),
            ("keepsItself", &Value::Null),
            ("label", &Value::Null),
            ("all", &Value::Null),
        ]
    );

    let (output, outline) =
        outline_of_source("Report.sharp", "public class Report\n{\n    public Report copy() => this;\n}\n");

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(outline["errors"], json!([]), "{outline}");
    assert_eq!(outline["symbols"][0]["children"][0]["returnType"], "Report");
}

#[test]
fn each_call_in_a_chain_names_the_call_it_is_made_on() {
    let (output, outline) = outline_of_source(
        "app/Shop/Checkout.sharp",
        "namespace Shop;\n\nimport App.Http.Client;\n\npublic class Checkout\n{\n    public void pay() => Client.make().post().json();\n}\n",
    );
    let calls: Vec<(&str, &Value)> = outline["references"]
        .as_array()
        .unwrap()
        .iter()
        .map(|reference| (reference["name"].as_str().unwrap(), &reference["receiverCall"]))
        .collect();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(calls, [("make", &Value::Null), ("post", &json!(0)), ("json", &json!(1))]);
}

#[test]
fn a_file_with_a_missing_brace_prints_the_symbols_before_it_and_its_error() {
    let (output, outline) = outline_of_source(
        "app/Shop/Cart.sharp",
        "namespace Shop;\n\npublic class Cart\n{\n    public int total() => 1;\n}\n\npublic class Order\n{\n    public void clear()\n    {\n        this.total();\n}\n",
    );
    let symbols = outline["symbols"].as_array().unwrap();

    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(symbols.len(), 1, "{outline}");
    assert_eq!(symbols[0]["name"], "Cart");
    assert_eq!(symbols[0]["children"][0]["name"], "total");
    assert_eq!(
        outline["errors"],
        json!([{"range": range(12, 1, 12, 1), "message": "Unexpected end of file"}]),
        "{outline}"
    );
}

#[test]
fn a_character_counts_its_utf_16_code_units() {
    let (output, outline) =
        outline_of_source("app/Shop/Cart.sharp", "namespace Shop;\n\n/* é 😀 */ public class Cart\n{\n}\n");

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(outline["symbols"][0]["range"], range(2, 11, 4, 1));
    assert_eq!(outline["symbols"][0]["selectionRange"], range(2, 24, 2, 28));
}

#[test]
fn a_file_it_cannot_read_prints_nothing_and_fails_with_the_reason() {
    let folder = tempfile::tempdir().unwrap();
    let output = process::output(
        Command::new(env!("CARGO_BIN_EXE_mago")).args(["outline", "app/Shop/Missing.sharp"]).current_dir(folder.path()),
    )
    .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("can't read app/Shop/Missing.sharp"));
}

#[test]
fn a_batch_prints_each_file_in_the_order_given() {
    let paths = [
        "sharp/mago/tests/fixtures/outline/Store.sharp",
        "sharp/mago/tests/fixtures/outline/Date.sharp",
        "Zend/tests/sharp/Generics/Catalog.sharp",
    ];

    for threads in ["1", "4"] {
        let output = process::output(
            Command::new(env!("CARGO_BIN_EXE_mago"))
                .args(["outline", "--json", "--threads", threads])
                .args(paths)
                .current_dir(repository_file("")),
        )
        .unwrap();
        let printed: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap()["path"].as_str().unwrap().to_owned())
            .collect();

        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(printed, paths, "--threads {threads}");
    }
}
