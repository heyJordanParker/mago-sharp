//! A goal a law's proof left open, printed in PHP#: the tree `Sharp/Lean/Proof.lean` writes of the Lean goal, read back
//! through the names the translation gave the code.

use foldhash::HashMap;
use foldhash::HashSet;
use serde_json::Value;

/// How tightly a PHP# expression binds, loosest first.
const TERNARY: u8 = 1;
const OR: u8 = 2;
const AND: u8 = 3;
const EQUALITY: u8 = 4;
const COMPARISON: u8 = 5;
const SUM: u8 = 6;
const PRODUCT: u8 = 7;
const PREFIX: u8 = 8;
const POSTFIX: u8 = 9;

/// The names a goal is printed with.
pub(crate) struct Names<'names> {
    /// Each member PHP# calls on a value, by its Lean name, and whether it is a property.
    pub(crate) receivers: &'names HashMap<String, bool>,
    /// The Lean name of every declaration the translation wrote, which PHP# calls on its class.
    pub(crate) declared: &'names HashSet<String>,
    /// The PHP# of each argument of the call a condition belongs to, which its goal names as `$argN`.
    pub(crate) arguments: &'names [String],
}

/// An expression as PHP# writes it, and how tightly it binds.
#[derive(Clone)]
struct Php {
    text: String,
    binds: u8,
}

impl Php {
    fn new(text: impl Into<String>, binds: u8) -> Self {
        Self { text: text.into(), binds }
    }

    /// The expression where an operand that binds at least `binds` stands: in parentheses when it binds looser.
    fn at(&self, binds: u8) -> String {
        if self.binds < binds { format!("({})", self.text) } else { self.text.clone() }
    }
}

/// `tree`, a goal `Sharp/Lean/Proof.lean` wrote as JSON, in PHP#, or `None` when it is no tree or holds a part PHP#
/// has no expression for.
pub(crate) fn php(tree: &str, names: &Names<'_>) -> Option<String> {
    let tree: Value = serde_json::from_str(tree).ok()?;
    let mut printer = Printer { names, bound: Vec::new(), complete: true };
    let goal = printer.print(&tree);

    printer.complete.then_some(goal.text)
}

/// The PHP# of each goal of a condition `tree` holds, joined by `&&`.
pub(crate) fn conjunction(tree: &str, names: &Names<'_>) -> Option<String> {
    let Value::Array(goals) = serde_json::from_str(tree).ok()? else {
        return None;
    };
    let mut printer = Printer { names, bound: Vec::new(), complete: true };
    let parts: Vec<String> = goals.iter().map(|goal| printer.print(goal).at(AND)).collect();

    printer.complete.then(|| parts.join(" && "))
}

struct Printer<'printer, 'names> {
    names: &'printer Names<'names>,
    /// Each value a step bound, innermost last, by the name its body reads it by.
    bound: Vec<(String, Php)>,
    /// Whether every part of the goal so far has a PHP# expression.
    complete: bool,
}

impl Printer<'_, '_> {
    fn print(&mut self, tree: &Value) -> Php {
        let Some(parts) = tree.as_array() else {
            return self.unknown();
        };
        let text = |index: usize| parts.get(index).and_then(Value::as_str).unwrap_or_default();
        match text(0) {
            "var" => self.variable(text(1)),
            "lit" => Php::new(text(1), POSTFIX + 1),
            "str" => Php::new(quoted(text(1)), POSTFIX + 1),
            "field" => {
                let value = self.print(&parts[2]);
                Php::new(format!("{}.{}", value.at(POSTFIX), text(1)), POSTFIX)
            }
            "let" => self.bind(text(1), &parts[2], &parts[3]),
            "app" => self.application(text(1), &parts[2..]),
            _ => self.unknown(),
        }
    }

    fn variable(&self, name: &str) -> Php {
        if let Some((_, value)) = self.bound.iter().rev().find(|(bound, _)| bound == name) {
            return value.clone();
        }
        if let Some(argument) = name.strip_prefix("$arg").and_then(|index| index.parse::<usize>().ok())
            && let Some(written) = self.names.arguments.get(argument.wrapping_sub(1))
        {
            let binds = if written.contains(' ') { TERNARY } else { POSTFIX };
            return Php::new(written.clone(), binds);
        }

        Php::new(name, POSTFIX + 1)
    }

    /// `body` with `name` standing for `value`, as a step binds it.
    fn bind(&mut self, name: &str, value: &Value, body: &Value) -> Php {
        let value = self.print(value);
        self.bound.push((name.to_owned(), value));
        let printed = self.print(body);
        self.bound.pop();

        printed
    }

    fn application(&mut self, function: &str, arguments: &[Value]) -> Php {
        match (function, arguments) {
            ("Sharp.holds" | "Pure.pure" | "Decidable.decide" | "Option.some", [value]) => self.print(value),
            // A law reads an int as the exact `Int` it holds, and checks one it passes on to code into the int range.
            ("Int64.toInt" | "Sharp.int.ofInt" | "Nat.cast", [value]) => self.print(value),
            ("Bind.bind", [value, lambda]) => match lambda.as_array().map(Vec::as_slice) {
                Some([kind, name, body]) if kind == "lam" => self.bind(name.as_str().unwrap_or_default(), value, body),
                _ => self.unknown(),
            },
            ("ite", [condition, then, otherwise]) => {
                let (condition, then, otherwise) = (self.print(condition), self.print(then), self.print(otherwise));
                Php::new(format!("{} ? {} : {}", condition.at(OR), then.at(OR), otherwise.at(TERNARY)), TERNARY)
            }
            ("Eq", [left, right]) if is(right, "Bool.true") => self.print(left),
            ("Eq", [left, right]) if is(right, "Bool.false") => self.not(left),
            ("Not" | "not", [value]) => self.not(value),
            ("Neg.neg" | "Sharp.int.neg", [value]) => {
                let value = self.print(value);
                Php::new(format!("-{}", value.at(PREFIX)), PREFIX)
            }
            ("List.nil", []) => Php::new("[]", POSTFIX + 1),
            ("List.cons", [_, _]) => self.list(arguments),
            ("List.append", [left, right]) => {
                let (left, right) = (self.print(left), self.print(right));
                Php::new(format!("[...{}, ...{}]", left.at(TERNARY), right.at(TERNARY)), POSTFIX + 1)
            }
            ("List.length", [list]) => {
                let list = self.print(list);
                Php::new(format!("{}.count()", list.at(POSTFIX)), POSTFIX)
            }
            ("Bool.true", []) => Php::new("true", POSTFIX + 1),
            ("Bool.false", []) => Php::new("false", POSTFIX + 1),
            ("Option.none", []) => Php::new("null", POSTFIX + 1),
            (operator, [left, right]) if binary(operator).is_some() => {
                let (symbol, binds) = binary(operator).unwrap_or_default();
                let (left, right) = (self.print(left), self.print(right));
                Php::new(format!("{} {symbol} {}", left.at(binds), right.at(binds + 1)), binds)
            }
            (member, _) => self.member(member, arguments),
        }
    }

    /// `!value`, or the comparison `value` is turned around.
    fn not(&mut self, value: &Value) -> Php {
        if let Some([kind, operator, left, right]) = value.as_array().map(Vec::as_slice)
            && kind == "app"
            && let Some(turned) = match operator.as_str() {
                Some("LE.le") => Some("GT.gt"),
                Some("LT.lt") => Some("GE.ge"),
                Some("GE.ge") => Some("LT.lt"),
                Some("GT.gt") => Some("LE.le"),
                Some("Eq" | "BEq.beq") => Some("Ne"),
                _ => None,
            }
        {
            return self.application(turned, &[left.clone(), right.clone()]);
        }
        let value = self.print(value);

        Php::new(format!("!{}", value.at(PREFIX)), PREFIX)
    }

    /// `[a, b]`, or `[a, ...rest]` when the list does not end in `[]`.
    fn list(&mut self, arguments: &[Value]) -> Php {
        let mut elements = Vec::new();
        let mut rest = arguments;
        while let [head, tail] = rest {
            elements.push(self.print(head).at(TERNARY));
            match tail.as_array().map(Vec::as_slice) {
                Some([kind, function, inner @ ..]) if kind == "app" && function == "List.cons" => rest = inner,
                Some([kind, function]) if kind == "app" && function == "List.nil" => break,
                _ => {
                    elements.push(format!("...{}", self.print(tail).at(TERNARY)));
                    break;
                }
            }
        }

        Php::new(format!("[{}]", elements.join(", ")), POSTFIX + 1)
    }

    /// A member of the code: called on its first argument when PHP# calls it on a value, `new Class(…)` for a
    /// constructor, and else on its class.
    fn member(&mut self, member: &str, arguments: &[Value]) -> Php {
        let mut parts = member.rsplitn(3, '.');
        let (name, class) = (parts.next().unwrap_or(member), parts.next().unwrap_or_default());
        let printed: Vec<Php> = arguments.iter().map(|argument| self.print(argument)).collect();
        let list =
            |arguments: &[Php]| arguments.iter().map(|argument| argument.at(TERNARY)).collect::<Vec<_>>().join(", ");

        if name == "__construct" {
            return Php::new(format!("new {class}({})", list(&printed)), POSTFIX);
        }
        match (self.names.receivers.get(member), printed.split_first()) {
            (Some(true), Some((receiver, _))) => Php::new(format!("{}.{name}", receiver.at(POSTFIX)), POSTFIX),
            (Some(false), Some((receiver, rest))) => {
                Php::new(format!("{}.{name}({})", receiver.at(POSTFIX), list(rest)), POSTFIX)
            }
            _ if !self.names.declared.contains(member) => self.unknown(),
            _ if printed.is_empty() => Php::new(format!("{class}.{name}"), POSTFIX),
            _ => Php::new(format!("{class}.{name}({})", list(&printed)), POSTFIX),
        }
    }

    /// A part of a goal PHP# has no expression for, which leaves the whole goal unprinted.
    fn unknown(&mut self) -> Php {
        self.complete = false;

        Php::new(String::new(), POSTFIX + 1)
    }
}

/// The PHP# operator a Lean function is, and how tightly it binds.
fn binary(function: &str) -> Option<(&'static str, u8)> {
    Some(match function {
        "Or" | "or" | "Bool.or" => ("||", OR),
        "And" | "and" | "Bool.and" => ("&&", AND),
        "Eq" | "BEq.beq" | "Iff" => ("==", EQUALITY),
        "Ne" | "bne" => ("!=", EQUALITY),
        "LE.le" => ("<=", COMPARISON),
        "LT.lt" => ("<", COMPARISON),
        "GE.ge" => (">=", COMPARISON),
        "GT.gt" => (">", COMPARISON),
        "HAdd.hAdd" | "HAppend.hAppend" | "Sharp.int.add" => ("+", SUM),
        "HSub.hSub" | "Sharp.int.sub" => ("-", SUM),
        "HMul.hMul" | "Sharp.int.mul" => ("*", PRODUCT),
        "HDiv.hDiv" | "Sharp.int.div" => ("/", PRODUCT),
        "HMod.hMod" | "Sharp.int.mod" => ("%", PRODUCT),
        _ => return None,
    })
}

/// Whether `tree` is the constant `name`.
fn is(tree: &Value, name: &str) -> bool {
    tree.as_array().is_some_and(|parts| parts.len() == 2 && parts[0] == "app" && parts[1] == name)
}

/// `text` as a PHP# string literal.
fn quoted(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The members of spec section 28.1's `Money` that its laws reach.
    const DECLARED: [&str; 5] = [
        "App.Shared.Money.__construct",
        "App.Shared.Money.add",
        "App.Shared.Money.times",
        "App.Shared.Money.total",
        "App.Shared.Money.refundable",
    ];

    fn printed(tree: &str, receivers: &[(&str, bool)], arguments: &[&str]) -> Option<String> {
        let receivers: HashMap<String, bool> =
            receivers.iter().map(|(name, property)| ((*name).to_owned(), *property)).collect();
        let declared: HashSet<String> = DECLARED.iter().map(|name| (*name).to_owned()).collect();
        let arguments: Vec<String> = arguments.iter().map(|argument| (*argument).to_owned()).collect();

        php(tree, &Names { receivers: &receivers, declared: &declared, arguments: &arguments })
    }

    /// The goal of `timesKeepsCurrency` without its call of `addKeepsCurrency`, as Lean leaves it once `times` is
    /// unfolded for `n > 0`, names each bound value where it is used.
    #[test]
    fn a_goal_reads_as_the_php_sharp_expression_it_stands_for() {
        let goal = r#"["app","Sharp.holds",["app","Bind.bind",["app","App.Shared.Money.times",["var","m"],["app","HSub.hSub",["var","n"],["lit","1"]]],["lam","x",["app","Bind.bind",["app","App.Shared.Money.add",["var","m"],["var","x"]],["lam","$1",["app","Pure.pure",["app","BEq.beq",["field","currency",["var","$1"]],["field","currency",["var","m"]]]]]]]]]"#;
        let receivers = [("App.Shared.Money.times", false), ("App.Shared.Money.add", false)];

        assert_eq!(printed(goal, &receivers, &[]).as_deref(), Some("m.add(m.times(n - 1)).currency == m.currency"));
    }

    #[test]
    fn a_static_call_a_constructor_and_operators_keep_their_php_sharp_form() {
        let goal = r#"["app","Not",["app","LE.le",["app","App.Shared.Money.refundable",["var","paid"],["app","Sharp.int.sub",["var","a"],["var","b"]]],["app","App.Shared.Money.__construct",["lit","0"],["str","EUR"]]]]"#;

        assert_eq!(printed(goal, &[], &[]).as_deref(), Some("Money.refundable(paid, a - b) > new Money(0, \"EUR\")"));
    }

    #[test]
    fn a_condition_names_the_arguments_of_its_call() {
        let condition = r#"[["app","Eq",["app","Decidable.decide",["app","LE.le",["lit","0"],["var","$arg1"]]],["app","Bool.true"]]]"#;
        let (receivers, declared) = (HashMap::default(), HashSet::default());
        let arguments = vec!["p".to_owned()];

        assert_eq!(
            conjunction(condition, &Names { receivers: &receivers, declared: &declared, arguments: &arguments })
                .as_deref(),
            Some("0 <= p")
        );
    }

    /// A goal that holds a part only Lean writes, as the `match` of `total` that Lean names `total.match_1`, has no
    /// PHP# form, so it is not printed.
    #[test]
    fn a_goal_with_a_part_php_sharp_has_no_expression_for_is_not_printed() {
        let goal = r#"["app","BEq.beq",["app","App.Shared.Money.total.match_1",["?"],["var","b"]],["app","App.Shared.Money.total",["var","b"]]]"#;

        assert_eq!(printed(goal, &[], &[]), None);
        assert_eq!(
            printed(r#"["app","BEq.beq",["app","App.Shared.Money.total",["var","b"]],["lit","0"]]"#, &[], &[])
                .as_deref(),
            Some("Money.total(b) == 0")
        );
    }
}
