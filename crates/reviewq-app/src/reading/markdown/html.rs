//! Project the parsed HTML tree of Markdown containing authored HTML.

use scraper::{ElementRef, Html, Node};

pub(super) fn project(source: &str, max_depth: usize, max_nodes: usize) -> Option<String> {
    let mut options = markdown::Options::gfm();
    options.compile.allow_dangerous_html = true;
    // The intermediate HTML stays in this process. Only display text leaves the service.
    let markup = markdown::to_html_with_options(source, &options).ok()?;
    let html = Html::parse_fragment(&markup);
    let mut output = String::new();
    let mut remaining = max_nodes;
    append(html.root_element(), &mut output, max_depth, &mut remaining)?;
    Some(output)
}

fn append(
    element: ElementRef<'_>,
    output: &mut String,
    depth: usize,
    remaining: &mut usize,
) -> Option<()> {
    let depth = depth.checked_sub(1)?;
    let name = element.value().name();
    match name {
        // Match the sanitizer's stripped script bodies and the parser's inert templates.
        "script" | "template" => return Some(()),
        "img" => {
            let alt = element
                .value()
                .attr("alt")
                .filter(|value| !value.is_empty());
            output.push_str(alt.unwrap_or("Attachment"));
            return Some(());
        }
        "pre"
            if element.child_elements().any(|child| {
                child.value().name() == "code"
                    && child
                        .value()
                        .classes()
                        .any(|class| class == "language-mermaid")
            }) =>
        {
            return Some(());
        }
        _ => (),
    }
    let mut first_cell = true;
    for child in element.children() {
        *remaining = remaining.checked_sub(1)?;
        if name == "tr" {
            if let Some(cell) =
                ElementRef::wrap(child).filter(|node| matches!(node.value().name(), "td" | "th"))
            {
                if !first_cell {
                    output.push('\t');
                }
                first_cell = false;
                append(cell, output, depth, remaining)?;
            }
        } else if let Node::Text(text) = child.value() {
            output.push_str(text);
        } else if let Some(child) = ElementRef::wrap(child) {
            append(child, output, depth, remaining)?;
        }
    }
    Some(())
}
