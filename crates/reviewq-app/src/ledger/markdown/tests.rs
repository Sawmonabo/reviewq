//! The field rules and the heading shift on the cases that once went wrong.

use super::*;

#[test]
fn stand_alone_fields() {
    let refused = |line, kind| Err(FieldProblem { line, kind });
    let cases = [
        (
            "an open top-level fence",
            MarkdownField::Details,
            "Setup.\n\n```rust\nlet retry = 1;\n",
            refused(3, ProblemKind::OpenFence),
        ),
        (
            "an open fence inside a list item",
            MarkdownField::Details,
            "Steps.\n\n- run\n  ```\n  cargo test\n",
            Ok(()),
        ),
        (
            "an unclosed HTML comment",
            MarkdownField::Evidence,
            "Seen in the log.\n\n<!-- trimmed output\n",
            refused(3, ProblemKind::OpenHtml),
        ),
        (
            "a summary paragraph that starts with Scope:",
            MarkdownField::Summary,
            "Adds retries to invoice writes.\n\nScope: the invoice path only.",
            refused(3, ProblemKind::LabelStart("Scope:")),
        ),
        (
            "a details paragraph that starts with a card label",
            MarkdownField::Details,
            "Context.\n\n**Proposed comment — reply in thread:** Ship it.",
            refused(3, ProblemKind::CardLabel(CardLabel::ProposedComment)),
        ),
        (
            "a footnote definition",
            MarkdownField::Note,
            "Retries are unbounded.\n\n[^1]: Seen in the worker log.\n",
            refused(3, ProblemKind::Footnote),
        ),
        (
            "a publication note that begins with a list",
            MarkdownField::Publication,
            "- Posted F-01 on the merge request.\n",
            refused(1, ProblemKind::JoinsList),
        ),
        (
            "details that begin with a sentence and then a list",
            MarkdownField::Details,
            "The replay steps:\n\n- drop the reply\n- retry\n",
            Ok(()),
        ),
        (
            "a summary paragraph that reads as the counts line",
            MarkdownField::Summary,
            "Adds retries.\n\nP0: 1 · P1: 0 · P2: 0 · P3: 0 · Blocking: 1 · Nonblocking: *0*",
            refused(3, ProblemKind::CountsLine),
        ),
        (
            "a rationale that states the counts within a sentence",
            MarkdownField::Rationale,
            "One finding blocks: P0: 1 · P1: 0 · P2: 0 · P3: 0 · Blocking: 1 · Nonblocking: 0",
            Ok(()),
        ),
    ];
    for (case, field, text, expected) in cases {
        assert_eq!(check_field(field, text), expected, "{case}");
    }
    let shifted = [
        (
            "a setext heading is shifted",
            "Intro.\n\nPlan\n====\n\nBody.\n",
            "Intro.\n\n#### Plan\n\nBody.\n",
        ),
        (
            "a quoted setext heading over two lines keeps one quote marker",
            "> *foo\n> bar*\n> ===",
            "> #### *foo bar*",
        ),
    ];
    for (case, text, expected) in shifted {
        assert_eq!(shift_headings(text), expected, "{case}");
    }
}

#[test]
fn flow_and_changes_hold_what_the_page_draws() {
    let refused = |line, kind| Err(FieldProblem { line, kind });
    let before_after =
        "| Before | After |\n| --- | --- |\n| Reads input unchecked. | Checks it first. |\n";
    let operations = "| Affected operation | What changes | Source |\n| --- | --- | --- |\n| `Parser::read` | Checks first. | `src/parser.rs` |\n";
    let cases = [
        (
            "a diagram inside a list item",
            MarkdownField::Flow,
            "The request path:\n\n- Client side:\n\n  ```mermaid\n  flowchart LR\n    A --> B\n  ```\n".to_owned(),
            Ok(()),
        ),
        (
            "a fence indented as code",
            MarkdownField::Flow,
            "The request path:\n\n    ```mermaid\n    flowchart LR\n      A --> B\n    ```\n".to_owned(),
            refused(3, ProblemKind::IndentedFence),
        ),
        (
            "only diagrams the page shows as source",
            MarkdownField::Flow,
            "```mermaid\nflowchart LR\n  A --> B\n  click A \"https://example.com\"\n```\n\n```mermaid\n%%{init: {}}%%\nflowchart LR\n  A --> B\n```\n".to_owned(),
            refused(1, ProblemKind::NotDrawn(Undrawable::Link)),
        ),
        (
            "one drawable diagram after one the page shows as source",
            MarkdownField::Flow,
            format!(
                "```mermaid\nflowchart LR\n{}```\n\n```mermaid\nflowchart LR\n  A --> B\n```\n",
                "  A --> B\n".repeat(501)
            ),
            Ok(()),
        ),
        (
            "Before and After, then the operations",
            MarkdownField::Changes,
            format!("{before_after}\n{operations}"),
            Ok(()),
        ),
        (
            "a lone table of another shape",
            MarkdownField::Changes,
            "| Area | Change |\n| --- | --- |\n| Parser | Checks first. |\n".to_owned(),
            refused(1, ProblemKind::NoBeforeAfter),
        ),
        (
            "Before and After without the operations",
            MarkdownField::Changes,
            before_after.to_owned(),
            refused(1, ProblemKind::NoOperations),
        ),
    ];
    for (case, field, text, expected) in cases {
        assert_eq!(check_field(field, &text), expected, "{case}");
    }
}
