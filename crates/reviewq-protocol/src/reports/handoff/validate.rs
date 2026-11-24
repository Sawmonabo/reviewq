use std::collections::BTreeSet;
use std::sync::LazyLock;

use jsonschema::{ValidationError, Validator, error::ValidationErrorKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::limits::{
    MAX_CONSEQUENCE_BYTES, MAX_DISCUSSION_ID_BYTES, MAX_EVIDENCE_BYTES, MAX_FIELD_ERRORS,
    MAX_ID_BYTES, MAX_PATH_BYTES, MAX_REASON_BYTES, MAX_SEVERITY_BYTES, MAX_TITLE_BYTES,
};
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

/// Validate public tool input before consuming its native workflow context. Lists at most
/// `MAX_FIELD_ERRORS` errors, then one `/handoff` error that counts the rest.
pub fn validate_report_handoff(input: &Value) -> Vec<ReportHandoffFieldError> {
    let mut errors = BTreeSet::new();
    for error in VALIDATOR.iter_errors(input) {
        collect(&error, &mut errors);
    }
    validate_values(input, &mut errors);
    let hidden = errors.len().saturating_sub(MAX_FIELD_ERRORS);
    let mut errors: Vec<_> = errors.into_iter().take(MAX_FIELD_ERRORS).collect();
    if hidden > 0 {
        errors.push(ReportHandoffFieldError {
            path: "/handoff".into(),
            message: hidden_errors(hidden),
        });
    }
    errors
}

/// Counts the errors a capped refusal leaves out.
fn hidden_errors(hidden: usize) -> String {
    if hidden == 1 {
        "1 more error is not shown. Fix these and submit again.".to_owned()
    } else {
        format!("{hidden} more errors are not shown. Fix these and submit again.")
    }
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
                // A name longer than any schema field is reported at its object, so the
                // reply never echoes an arbitrarily long key.
                let path = if key.len() <= MAX_ID_BYTES {
                    child(&path, key)
                } else {
                    path.clone()
                };
                record(errors, path, "Unexpected field for this outcome.");
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
    reason(errors, "/handoff/reason".into(), &handoff["reason"]);
    reason(
        errors,
        "/handoff/revision/reason".into(),
        &handoff["revision"]["reason"],
    );
    for (index, assessment) in entries(&handoff["assessments"]).iter().enumerate() {
        let path = format!("/handoff/assessments/{index}");
        reason(errors, child(&path, "reason"), &assessment["reason"]);
        bounded_id(
            errors,
            child(&path, "discussionId"),
            &assessment["discussionId"],
            MAX_DISCUSSION_ID_BYTES,
        );
        for (position, id) in entries(&assessment["findingIds"]).iter().enumerate() {
            bounded_id(
                errors,
                format!("{path}/findingIds/{position}"),
                id,
                MAX_ID_BYTES,
            );
        }
    }
    unique_ids(handoff, "assessments", "discussionId", errors);
    if handoff["outcome"] != "final" {
        return;
    }
    let report = &handoff["report"];
    match (handoff["amendsReport"].is_object(), report.is_null()) {
        (false, true) => record(errors, "/handoff/report", "Required."),
        (true, false) => record(
            errors,
            "/handoff/report",
            "Omit report when amendsReport is present.",
        ),
        _ => {}
    }
    if handoff["amendsReport"].is_object() {
        if !entries(&handoff["assessments"]).is_empty() {
            record(
                errors,
                "/handoff/amendsReport",
                "An amendment cannot complete a selected-thread check.",
            );
        }
        if !entries(&handoff["findingChanges"]).is_empty() {
            record(
                errors,
                "/handoff/findingChanges",
                "Omit findingChanges when amendsReport is present.",
            );
        }
    }
    if !entries(&handoff["assessments"]).is_empty() && handoff["revision"]["sha"].is_null() {
        record(
            errors,
            "/handoff/revision/sha",
            "A selected-thread result requires an exact reviewed SHA.",
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
    let mut retired = BTreeSet::new();
    for (index, change) in entries(&handoff["findingChanges"]).iter().enumerate() {
        let path = format!("/handoff/findingChanges/{index}");
        reason(errors, child(&path, "reason"), &change["reason"]);
        bounded_id(
            errors,
            child(&path, "findingId"),
            &change["findingId"],
            MAX_ID_BYTES,
        );
        let mut discussions = BTreeSet::new();
        for (position, id) in entries(&change["discussionIds"]).iter().enumerate() {
            let id_path = format!("{path}/discussionIds/{position}");
            if id.as_str().is_some_and(|id| !discussions.insert(id)) {
                record(
                    errors,
                    id_path.clone(),
                    "ID is already used in this collection.",
                );
            }
            bounded_id(errors, id_path, id, MAX_DISCUSSION_ID_BYTES);
        }
        if let Some(id) = change["findingId"].as_str()
            && change["conclusion"]
                .as_str()
                .is_some_and(|conclusion| RETIRING.contains(&conclusion))
        {
            retired.insert(id);
        }
    }
    unique_ids(handoff, "findingChanges", "findingId", errors);
    let mut ids = BTreeSet::new();
    for (index, finding) in entries(&report["findings"]).iter().enumerate() {
        let path = format!("/handoff/report/findings/{index}");
        if let Some(id) = finding["id"].as_str() {
            if !ids.insert(id) {
                record(
                    errors,
                    child(&path, "id"),
                    "Finding ID is already used in this handoff.",
                );
            }
            if retired.contains(id) && id.len() <= MAX_ID_BYTES {
                record(
                    errors,
                    child(&path, "id"),
                    format!(
                        "{id} is retired by findingChanges; remove it from report.findings or use a keeping outcome."
                    ),
                );
            }
        }
        for (field, value, limit) in [
            ("title", &finding["title"], MAX_TITLE_BYTES),
            ("severity", &finding["severity"], MAX_SEVERITY_BYTES),
            (
                "consequence",
                &finding["consequence"],
                MAX_CONSEQUENCE_BYTES,
            ),
            (
                "evidence/text",
                &finding["evidence"]["text"],
                MAX_EVIDENCE_BYTES,
            ),
            (
                "location/path",
                &finding["location"]["path"],
                MAX_PATH_BYTES,
            ),
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

const RETIRING: [&str; 3] = ["fixed", "clarified_withdrawn", "superseded"];

fn entries(value: &Value) -> &[Value] {
    value.as_array().map_or(&[], Vec::as_slice)
}

fn reason(errors: &mut BTreeSet<ReportHandoffFieldError>, path: String, value: &Value) {
    if value
        .as_str()
        .is_some_and(|value| value.trim().is_empty() || value.len() > MAX_REASON_BYTES)
    {
        record(
            errors,
            path,
            format!("Use a non-blank reason of at most {MAX_REASON_BYTES} bytes."),
        );
    }
}

fn bounded_id(
    errors: &mut BTreeSet<ReportHandoffFieldError>,
    path: String,
    value: &Value,
    limit: usize,
) {
    if value.as_str().is_some_and(|value| value.len() > limit) {
        record(errors, path, format!("Use an ID of at most {limit} bytes."));
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
        input["handoff"]["report"]["findings"] = json!([card, {"id":"F-01","blocking":"yes"}]);
        input["handoff"]["report"]["findings"][0]["location"]["line"] = json!(4294967296_u64);
        input["handoff"]["findingChanges"] = json!([{
            "findingId":"F-01","conclusion":"fixed","reason":"The retry now reuses its key."
        }]);
        input["handoff"]["source"] = json!({"kind":"file","sha256":false});
        let errors = validate_report_handoff(&input);
        for path in [
            "/handoff/report/version",
            "/handoff/report/findings/0/id",
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
        assert!(errors.iter().any(|error| error.message
            == "F-01 is retired by findingChanges; remove it from report.findings or use a keeping outcome."));
        input["handoff"]["report"] = Value::Null;
        assert!(
            validate_report_handoff(&input)
                .iter()
                .any(|error| error.path == "/handoff/report" && error.message == "Required.")
        );
        assert!(!validate_report_handoff(&json!({"handoff":{"outcome":"needs_input","reason":"Awaiting target revision","verdict":"approve"}})).is_empty());
    }

    #[test]
    fn a_metadata_amendment_uses_the_accepted_file_digest() {
        let mut input: Value = serde_json::from_str(REPORT_HANDOFF_EXAMPLE).unwrap();
        let accepted = "a".repeat(64);
        let changed = "b".repeat(64);
        let handoff = &mut input["handoff"];
        let report = handoff.as_object_mut().unwrap().remove("report").unwrap();
        handoff["amendsReport"] = json!({"receiptId":"accepted-receipt","sha256":accepted});
        handoff["source"] = json!({"kind":"file","path":"/tmp/report.md","sha256":accepted});
        assert!(validate_report_handoff(&input).is_empty());
        assert!(serde_json::from_value::<ReportHandoffInput>(input.clone()).is_ok());

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

        input["handoff"]["source"]["sha256"] = json!(accepted);
        input["handoff"]["report"] = report;
        assert_eq!(
            validate_report_handoff(&input),
            vec![ReportHandoffFieldError {
                path: "/handoff/report".into(),
                message: "Omit report when amendsReport is present.".into(),
            }]
        );
        assert!(serde_json::from_value::<ReportHandoffInput>(input.clone()).is_err());

        input["handoff"].as_object_mut().unwrap().remove("report");
        input["handoff"]["assessments"] = json!([{
            "discussionId":"discussion-1","conclusion":"fixed","reason":"The key is now scoped.",
            "findingIds":["F-01"],"nextAction":"review"
        }]);
        input["handoff"]["findingChanges"] = json!([{
            "findingId":"F-01","conclusion":"fixed","reason":"The key is now scoped."
        }]);
        assert_eq!(
            validate_report_handoff(&input),
            vec![
                ReportHandoffFieldError {
                    path: "/handoff/amendsReport".into(),
                    message: "An amendment cannot complete a selected-thread check.".into(),
                },
                ReportHandoffFieldError {
                    path: "/handoff/findingChanges".into(),
                    message: "Omit findingChanges when amendsReport is present.".into(),
                },
            ]
        );
    }
}
