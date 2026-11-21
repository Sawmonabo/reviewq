use std::collections::HashSet;

const SVG: &str = "http://www.w3.org/2000/svg";
const ID: &str = "reviewq-saved-drawing";

pub(super) fn valid(source: &str) -> bool {
    let options = roxmltree::ParsingOptions {
        nodes_limit: 16_384,
        ..roxmltree::ParsingOptions::default()
    };
    let Ok(document) = roxmltree::Document::parse_with_options(source, options) else {
        return false;
    };
    let root = document.root_element();
    if !root.has_tag_name((SVG, "svg"))
        || root.attribute("id") != Some(ID)
        || document.descendants().any(|node| node.is_pi())
    {
        return false;
    }
    let view_box: Vec<_> = root
        .attribute("viewBox")
        .unwrap_or_default()
        .split(|ch: char| ch == ',' || ch.is_ascii_whitespace())
        .filter(|value| !value.is_empty())
        .map(str::parse::<f64>)
        .collect::<Result<_, _>>()
        .unwrap_or_default();
    if view_box.len() != 4
        || view_box.iter().any(|value| !value.is_finite())
        || view_box[2] <= 0.0
        || view_box[3] <= 0.0
    {
        return false;
    }
    let mut ids = HashSet::new();
    for node in document.descendants().filter(roxmltree::Node::is_element) {
        let tag = node.tag_name();
        let tag_name = tag.name().to_ascii_lowercase();
        if tag.namespace() != Some(SVG)
            || tag_name.starts_with("animate")
            || matches!(
                tag_name.as_str(),
                "script"
                    | "foreignobject"
                    | "image"
                    | "feimage"
                    | "a"
                    | "set"
                    | "discard"
                    | "handler"
                    | "audio"
                    | "video"
                    | "iframe"
                    | "link"
            )
            || node
                .attribute("id")
                .is_some_and(|id| !id.starts_with(ID) || !ids.insert(id))
        {
            return false;
        }
    }
    for node in document.descendants().filter(roxmltree::Node::is_element) {
        if node.attributes().any(|attribute| {
            let name = attribute.name().to_ascii_lowercase();
            let value = attribute.value();
            name.starts_with("on")
                || name.starts_with("src")
                || matches!(name.as_str(), "base" | "for" | "data")
                || (name == "href" && !value.strip_prefix('#').is_some_and(|id| ids.contains(id)))
                || ((name == "style" || value.to_ascii_lowercase().contains("url("))
                    && !local_style(value, &ids))
        }) || (node.has_tag_name("style") && node.children().any(|child| !child.is_text())
            || node.has_tag_name("style") && !local_style(node.text().unwrap_or_default(), &ids))
        {
            return false;
        }
    }
    true
}

fn local_style(style: &str, ids: &HashSet<&str>) -> bool {
    let lower = style.to_ascii_lowercase();
    if lower.contains(['\\', '<'])
        || [
            "@import",
            "@font-face",
            "http:",
            "https:",
            "image-set",
            "/*",
        ]
        .iter()
        .any(|pattern| lower.contains(pattern))
    {
        return false;
    }
    let mut rest = style;
    while let Some(open) = rest.to_ascii_lowercase().find("url(") {
        let Some((target, tail)) = rest[open + 4..].split_once(')') else {
            return false;
        };
        let target = target.trim();
        let target = target
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .or_else(|| target.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
            .unwrap_or(target);
        if !target.strip_prefix('#').is_some_and(|id| ids.contains(id)) {
            return false;
        }
        rest = tail;
    }
    true
}
