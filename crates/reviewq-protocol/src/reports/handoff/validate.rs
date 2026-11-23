use std::collections::BTreeSet;
use std::sync::LazyLock;

use jsonschema::{ValidationError, Validator, error::ValidationErrorKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{REPORT_AMENDMENT_SOURCE_MESSAGE, REPORT_HANDOFF_SCHEMA, ReportHashDto};

static VALIDATOR: LazyLock<Validator> = LazyLock::new(|| {
    let schema =
        serde_json::from_str(REPORT_HANDOFF_SCHEMA).expect("generated handoff schema JSON");
    jsonschema::validator_for(&schema).expect("generated handoff schema compiles")
});

#[derive(Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ReportHandoffFieldError {
    pub path: String,
    pub message: String,
}

pub fn report_amendment_source_error() -> ReportHandoffFieldError {
    ReportHandoffFieldError {
        path: "/handoff/source/sha256".into(),
        message: REPORT_AMENDMENT_SOURCE_MESSAGE.into(),
    }
}

/// Validate public tool input before consuming its native workflow context.
pub fn validate_report_handoff(input: &Value) -> Vec<ReportHandoffFieldError> {
    let mut errors = BTreeSet::new();
    for error in VALIDATOR.iter_errors(input) {
        collect(&error, &mut errors);
    }
    validate_values(input, &mut errors);
    errors.into_iter().collect()
}

fn collect(error: &ValidationError<'_>, errors: &mut BTreeSet<ReportHandoffFieldError>) {
    use ValidationErrorKind as Kind;
    let path = error.instance_path().to_string();
    match error.kind() {
        Kind::Required { property } => record(
            errors,
            child(&path, property.as_str().unwrap_or("")),
            "Required.",
        ),
        Kind::AdditionalProperties { unexpected } => {
            for key in unexpected {
                record(
                    errors,
                    child(&path, key),
                    "Unexpected field for this outcome.",
                );
            }
        }
        Kind::OneOfNotValid { context } | Kind::AnyOf { context } => {
            let instance = error.instance();
            let tag = match path.as_str() {
                "/handoff" => Some("outcome"),
                "/handoff/source" => Some("kind"),
                _ => None,
            };
            if let Some(tag) = tag.filter(|_| instance.is_object()) {
                let tag_path = child(&path, tag);
                if instance.get(tag).is_none() {
                    record(errors, tag_path, "Required.");
                    return;
                }
                if let Some(branch) = context.iter().find(|branch| {
                    !branch
                        .iter()
                        .any(|error| error.instance_path().to_string() == tag_path)
                }) {
                    for error in branch {
                        collect(error, errors);
                    }
                } else {
                    record(
                        errors,
                        tag_path,
                        "Use a value defined by the handoff schema.",
                    );
                }
            } else if let Some(branch) = context.iter().min_by_key(|branch| {
                let shape_mismatch = branch.iter().any(|error| {
                    matches!(error.kind(), Kind::Type { .. })
                        && error.instance_path().to_string() == path
                });
                (shape_mismatch, branch.len())
            }) {
                for error in branch {
                    collect(error, errors);
                }
            }
        }
        Kind::Enum { options } => record(errors, path, format!("Use one of {options}.")),
        Kind::Constant { expected_value } => record(errors, path, format!("Use {expected_value}.")),
        Kind::MaxItems { limit } => record(errors, path, format!("Use at most {limit} entries.")),
        Kind::MaxLength { limit } => {
            record(errors, path, format!("Use at most {limit} characters."))
        }
        Kind::MinLength { limit } => {
            record(errors, path, format!("Use at least {limit} characters."))
        }
        Kind::Maximum { limit } if path.ends_with("/version") => record(
            errors,
            path,
            format!("Use supported report version {limit}."),
        ),
        Kind::Minimum { limit } if path.ends_with("/version") => record(
            errors,
            path,
            format!("Use supported report version {limit}."),
        ),
        Kind::Maximum { limit } => record(errors, path, format!("Use at most {limit}.")),
        Kind::Minimum { limit } => record(errors, path, format!("Use at least {limit}.")),
        Kind::Pattern { .. } => record(errors, path, "Value does not match the required format."),
        Kind::Type { .. } => record(errors, path, "Value has the wrong JSON type."),
        _ => record(errors, path, "Value does not match the handoff schema."),
    }
}

fn validate_values(input: &Value, errors: &mut BTreeSet<ReportHandoffFieldError>) {
    let handoff = &input["handoff"];
    for (path, value) in [
        ("/handoff/reason", &handoff["reason"]),
        ("/handoff/revision/reason", &handoff["revision"]["reason"]),
    ] {
        if value
            .as_str()
            .is_some_and(|value| value.trim().is_empty() || value.len() > 1024)
        {
            record(
                errors,
                path,
                "Use a non-blank reason of at most 1024 bytes.",
            );
        }
    }
    unique_ids(handoff, "assessments", "discussionId", errors);
    if handoff["outcome"] != "final" {
        return;
    }
    let report = &handoff["report"];
    let scoped = handoff["assessments"]
        .as_array()
        .is_some_and(|items| !items.is_empty());
    let amendment = handoff["amendsReport"].is_object() || handoff["correctsReport"].is_object();
    let expected = match (scoped, amendment) {
        (true, false) => "selected_threads",
        (false, true) => "amendment",
        (false, false) => "full_review",
        (true, true) => {
            record(
                errors,
                "/handoff/report/scope",
                "A selected-thread result cannot amend a report.",
            );
            "selected_threads"
        }
    };
    if report["scope"]
        .as_str()
        .is_some_and(|scope| scope != expected)
    {
        record(
            errors,
            "/handoff/report/scope",
            format!("Use {expected} for the supplied handoff fields."),
        );
    }
    if scoped && handoff["revision"]["sha"].is_null() {
        record(
            errors,
            "/handoff/revision/sha",
            "A selected-thread result requires an exact reviewed SHA.",
        );
    }
    if handoff["amendsReport"].is_object() && handoff["correctsReport"].is_object() {
        record(
            errors,
            "/handoff/correctsReport",
            "Supply an amendment or a correction.",
        );
    }
    let hash = |value: &Value| value.as_str().and_then(ReportHashDto::new);
    if handoff["source"]["kind"] == "file"
        && let Some(file) = hash(&handoff["source"]["sha256"])
        && let Some(accepted) = hash(&handoff["amendsReport"]["sha256"])
        && file != accepted
    {
        errors.insert(report_amendment_source_error());
    }
    for field in ["findingChanges", "sectionChanges"] {
        if handoff[field]
            .as_array()
            .is_some_and(|items| !items.is_empty())
            && !scoped
            && !handoff["correctsReport"].is_object()
        {
            record(
                errors,
                child("/handoff", field),
                "Supply changes only for a selected-thread result or a correction.",
            );
        }
    }
    unique_ids(handoff, "findingChanges", "findingId", errors);
    let Some(findings) = report["findings"].as_array() else {
        return;
    };
    let mut ids = BTreeSet::new();
    for (index, finding) in findings.iter().enumerate() {
        let path = format!("/handoff/report/findings/{index}");
        if let Some(id) = finding["id"].as_str()
            && !ids.insert(id)
        {
            record(
                errors,
                child(&path, "id"),
                "Finding ID is already used in this handoff.",
            );
        }
        for (field, value, limit) in [
            ("title", &finding["title"], 384),
            ("severity", &finding["severity"], 64),
            ("consequence", &finding["consequence"], 1024),
            ("evidence/text", &finding["evidence"]["text"], 2048),
            ("location/path", &finding["location"]["path"], 512),
        ] {
            if value.as_str().is_some_and(|value| {
                value.trim().is_empty()
                    || value.len() > limit
                    || value.chars().any(|value| {
                        value.is_control()
                            && (field == "location/path" || !matches!(value, '\n' | '\r' | '\t'))
                    })
            }) {
                record(
                    errors,
                    format!("{path}/{field}"),
                    format!("Use non-blank text of at most {limit} bytes."),
                );
            }
        }
    }
}

fn unique_ids(
    handoff: &Value,
    collection: &str,
    field: &str,
    errors: &mut BTreeSet<ReportHandoffFieldError>,
) {
    let Some(items) = handoff[collection].as_array() else {
        return;
    };
    let mut seen = BTreeSet::new();
    for (index, item) in items.iter().enumerate() {
        if let Some(id) = item[field].as_str()
            && !seen.insert(id)
        {
            record(
                errors,
                format!("/handoff/{collection}/{index}/{field}"),
                "ID is already used in this collection.",
            );
        }
    }
}

fn child(path: &str, key: &str) -> String {
    format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"))
}

fn record(
    errors: &mut BTreeSet<ReportHandoffFieldError>,
    path: impl Into<String>,
    message: impl Into<String>,
) {
    errors.insert(ReportHandoffFieldError {
        path: path.into(),
        message: message.into(),
    });
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{ReportHandoffFieldError, validate_report_handoff};
    use crate::reports::{REPORT_HANDOFF_EXAMPLE, ReportHandoffInput};

    #[test]
    fn structured_examples_and_combined_field_errors() {
        let mut input: Value = serde_json::from_str(REPORT_HANDOFF_EXAMPLE).unwrap();
        assert!(validate_report_handoff(&input).is_empty());
        assert!(serde_json::from_value::<ReportHandoffInput>(input.clone()).is_ok());
        let card = input["handoff"]["report"]["findings"][0].clone();
        input["handoff"]["report"]["findings"] = json!([]);
        assert!(validate_report_handoff(&input).is_empty());

        input["handoff"]["report"]["version"] = json!(2);
        input["handoff"]["report"]["scope"] = json!("selected_threads");
        input["handoff"]["report"]["findings"] = json!([card, {"id":"F-1","blocking":"yes"}]);
        input["handoff"]["report"]["findings"][0]["location"]["line"] = json!(4294967296_u64);
        input["handoff"]["source"] = json!({"kind":"file","sha256":false});
        let errors = validate_report_handoff(&input);
        for path in [
            "/handoff/report/version",
            "/handoff/report/scope",
            "/handoff/report/findings/0/location/line",
            "/handoff/report/findings/1/id",
            "/handoff/report/findings/1/blocking",
            "/handoff/report/findings/1/consequence",
            "/handoff/report/findings/1/evidence",
            "/handoff/report/findings/1/severity",
            "/handoff/report/findings/1/title",
            "/handoff/source/path",
            "/handoff/source/sha256",
        ] {
            assert!(
                errors
                    .iter()
                    .any(|error: &ReportHandoffFieldError| error.path == path),
                "{path} missing in {errors:?}"
            );
        }
        input["handoff"]["report"] = Value::Null;
        assert!(
            validate_report_handoff(&input)
                .iter()
                .any(|error| error.path == "/handoff/report")
        );
        assert!(!validate_report_handoff(&json!({"handoff":{"outcome":"needs_input","reason":"Awaiting target revision","verdict":"approve"}})).is_empty());
    }

    #[test]
    fn a_metadata_amendment_uses_the_accepted_file_digest() {
        let mut input: Value = serde_json::from_str(REPORT_HANDOFF_EXAMPLE).unwrap();
        let accepted = "a".repeat(64);
        let changed = "b".repeat(64);
        let handoff = &mut input["handoff"];
        handoff["amendsReport"] = json!({"receiptId":"accepted-receipt","sha256":accepted});
        handoff["report"]["scope"] = json!("amendment");
        handoff["report"]["findings"] = json!([]);
        handoff["source"] = json!({"kind":"file","path":"/tmp/report.md","sha256":accepted});
        assert!(validate_report_handoff(&input).is_empty());

        input["handoff"]["source"]["sha256"] = json!(changed);
        let errors = validate_report_handoff(&input);
        assert_eq!(
            errors,
            vec![ReportHandoffFieldError {
                path: "/handoff/source/sha256".into(),
                message: super::REPORT_AMENDMENT_SOURCE_MESSAGE.into(),
            }]
        );
        let parsed: ReportHandoffInput = serde_json::from_value(input.clone()).unwrap();
        assert!(parsed.handoff.changes_amended_source());

        let handoff = input["handoff"].as_object_mut().unwrap();
        handoff.remove("amendsReport");
        handoff.insert(
            "correctsReport".into(),
            json!({
                "receiptId":"accepted-receipt","sha256":accepted,"kind":"evidence",
                "reason":"Clarify the cited evidence.","findingIds":["F-1"],
                "evidence":[{"findingId":"F-1","sha256":"c".repeat(64)}]
            }),
        );
        assert!(validate_report_handoff(&input).is_empty());
    }
}
