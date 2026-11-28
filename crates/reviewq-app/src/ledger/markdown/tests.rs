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

/// The text of a fixture body: a string, or a list of strings and `{"repeat", "times"}` pieces.
fn body(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(pieces) => pieces
            .iter()
            .map(|piece| match piece {
                serde_json::Value::String(text) => text.clone(),
                piece => piece["repeat"]
                    .as_str()
                    .expect("a repeated piece's text")
                    .repeat(piece["times"].as_u64().expect("a repeat count") as usize),
            })
            .collect(),
        other => panic!("a body is a string or a list of pieces, not {other}"),
    }
}

/// The shared cases of `fixtures/markdown/drawn-sections.json`, which the page's browser test
/// reads too: each heading is read as the page shows it, and each section is accepted exactly
/// when the page draws what it needs, with the diagram text the page hands Mermaid.
#[test]
fn drawn_sections_agree_with_the_page() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../fixtures/markdown/drawn-sections.json"
    ))
    .expect("the fixture is JSON");
    let operations = "| Affected operation | What changes | Source |\n| --- | --- | --- |\n| `Parser::read` | Checks first. | `src/parser.rs` |\n";
    for case in fixture["headings"].as_array().expect("heading cases") {
        let name = case["case"].as_str().expect("a case name");
        let shown = case["shown"].as_str().expect("the shown text");
        let table = format!(
            "| {} | After |\n| --- | --- |\n| Reads input unchecked. | Checks it first. |\n",
            case["cell"].as_str().expect("a cell")
        );
        let tables = content::top_level_tables(&table);
        assert_eq!(tables[0].headings[0], shown, "{name}");
        let checked = check_field(MarkdownField::Changes, &format!("{table}\n{operations}"));
        assert_eq!(checked.is_ok(), shown == "before", "{name}: {checked:?}");
    }
    for case in fixture["sections"].as_array().expect("section cases") {
        let name = case["case"].as_str().expect("a case name");
        let text = body(&case["body"]);
        let field = match case["section"].as_str() {
            Some("flow") => MarkdownField::Flow,
            Some("changes") => MarkdownField::Changes,
            other => panic!("{name}: no section {other:?}"),
        };
        let checked = check_field(field, &text);
        let expected = if case["drawn"] == true {
            Ok(())
        } else {
            let refusal = &case["refusal"];
            Err((
                refusal["line"].as_u64(),
                refusal["kind"].as_str().map(str::to_owned),
            ))
        };
        let found = checked.map_err(|problem| {
            (
                Some(problem.line as u64),
                Some(format!("{:?}", problem.kind)),
            )
        });
        assert_eq!(found, expected, "{name}");
        if let Some(diagrams) = case["diagrams"].as_array() {
            let diagrams: Vec<String> = diagrams.iter().map(body).collect();
            let sources = crate::reports::mermaid_sources(&text);
            assert_eq!(sources, diagrams, "{name}");
            if let Some(digests) = case["digests"].as_array() {
                let found: Vec<String> = sources
                    .iter()
                    .map(|source| crate::reports::drawing_digest(source).as_str().to_owned())
                    .collect();
                let digests: Vec<&str> = digests.iter().filter_map(|hex| hex.as_str()).collect();
                assert_eq!(found, digests, "{name}");
            }
        }
    }
}
