use std::collections::HashSet;
use std::fmt;
use std::fmt::Display;

use mago_bytes::BytesDisplay;
use mago_database::file::File;
use mago_names::ResolvedNames;
use mago_names::display_sharp_member;
use mago_names::short_name;
use mago_php_version::PHPVersion;
use mago_reporting::Issue;
use mago_reporting::IssueCollection;
use mago_span::HasSpan;
use mago_span::Position;
use mago_span::Span;
use mago_syntax::cst::Label;
use mago_syntax::cst::Node;
use mago_syntax::cst::Program;

use crate::internal::checker::sharp::Place;

const ISSUE_CODE: &str = "semantics";

#[derive(Debug)]
pub struct Context<'ctx, 'ast, 'arena> {
    pub version: PHPVersion,
    pub program: &'ast Program<'arena>,
    pub names: &'ast ResolvedNames<'arena>,
    pub source_file: &'ctx File,
    /// The statements and expressions the walk is inside, innermost last.
    pub ancestors: Vec<Node<'ast, 'arena>>,
    pub hint_depth: usize,
    /// Every `goto` label of the file, collected at its first `goto`.
    pub labels: Option<Vec<&'ast Label<'arena>>>,
    /// The name and body of the last namespace without braces the walk left.
    pub last_unbraced_namespace: Option<(Span, Span)>,
    /// The name and body of the last namespace in braces the walk left.
    pub last_braced_namespace: Option<(Span, Span)>,
    /// The slice's place for the children of each node the walk is inside, innermost last, or `None` inside a node
    /// the slice refused.
    pub slice_places: Vec<Option<Place>>,
    /// The member name spans of the member accesses the slice checked.
    pub slice_members: HashSet<Span>,

    issues: IssueCollection,
}

impl<'ctx, 'ast, 'arena> Context<'ctx, 'ast, 'arena> {
    pub fn new(
        version: PHPVersion,
        program: &'ast Program<'arena>,
        names: &'ast ResolvedNames<'arena>,
        source_file: &'ctx File,
    ) -> Self {
        Self {
            version,
            program,
            names,
            source_file,
            issues: IssueCollection::default(),
            ancestors: vec![],
            hint_depth: 0,
            labels: None,
            last_unbraced_namespace: None,
            last_braced_namespace: None,
            slice_places: vec![],
            slice_members: HashSet::new(),
        }
    }

    #[inline]
    pub fn get_name(&self, position: Position) -> &'arena [u8] {
        self.names.get(&position)
    }

    /// The class-like `full_name` as the checked file writes it: its short name in a `.sharp` file, and its full name in
    /// PHP. It is formatted only when a message prints it.
    pub fn display_class_like_name<'name>(&self, full_name: &'name [u8]) -> impl Display + use<'name> {
        let is_sharp = self.program.dialect.is_sharp();

        fmt::from_fn(move |formatter| {
            if is_sharp { formatter.write_str(&short_name(full_name)) } else { BytesDisplay(full_name).fmt(formatter) }
        })
    }

    /// The member `member_name` of the class-like `class_like_name` as the checked file names it: `Order.total` in a
    /// `.sharp` file, and `Order::total` in PHP, where a property keeps its `$`: `Order::$total`. It is formatted only
    /// when a message prints it.
    pub fn display_member<'name, M>(&self, class_like_name: &'name [u8], member_name: M) -> impl Display + use<'name, M>
    where
        M: Display,
    {
        let is_sharp = self.program.dialect.is_sharp();

        fmt::from_fn(move |formatter| {
            if is_sharp {
                formatter.write_str(&display_sharp_member(class_like_name, &member_name))
            } else {
                write!(formatter, "{}::{member_name}", BytesDisplay(class_like_name))
            }
        })
    }

    #[inline]
    pub fn get_code_snippet(&self, span: impl HasSpan) -> &'ctx [u8] {
        let s = span.span();

        &self.source_file.contents[s.start.offset as usize..s.end.offset as usize]
    }

    /// Reports a semantic issue with the given `Issue`.
    ///
    /// This method adds the issue to the context's issue collection,
    /// appending the `ISSUE_CODE` to the issue for identification.
    ///
    /// # Arguments
    ///
    /// `issue`: The `Issue` to report, which contains details about the semantic violation.
    pub fn report(&mut self, issue: Issue) {
        self.issues.push(issue.with_code(ISSUE_CODE));
    }

    /// Finalizes the context and returns the collected issues.
    ///
    /// This method is typically called at the end of the semantic analysis
    /// to retrieve all reported issues.
    pub fn finalize(self) -> IssueCollection {
        self.issues
    }
}
