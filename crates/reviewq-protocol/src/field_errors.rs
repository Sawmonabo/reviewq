//! Field errors a tool input's JSON Schema reports, one per field, and the one cap a reply
//! applies to them.

use std::collections::BTreeSet;

use jsonschema::{ValidationError, Validator, error::ValidationErrorKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::scalar::{FINDING_ID_PATTERN, GIT_SHA_PATTERN};

/// Field errors one reply lists before a single summary of the rest, which keeps a refusal
/// short enough to read and far below the ledger result limit.
pub const MAX_FIELD_ERRORS: usize = 50;
/// Longest unexpected field name an error repeats. Every schema field name is shorter.
const MAX_ECHOED_NAME_BYTES: usize = 64;

/// One refused field: its JSON Pointer and what to change.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct FieldError {
    pub path: String,
    pub message: String,
}

impl FieldError {
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

/// Adds one error per field that `validator` refuses in `input`. A `oneOf` or `anyOf` failure
/// reports the branch closest to the input's shape.
pub fn collect_schema_errors(
    validator: &Validator,
    input: &Value,
    errors: &mut BTreeSet<FieldError>,
) {
    for error in validator.iter_errors(input) {
        collect(&error, errors);
    }
}

/// Lists at most `MAX_FIELD_ERRORS` errors in field order, then one root error that counts the
/// rest. The transport calls it once on every refusal it returns, and nothing else caps.
pub fn bounded_field_errors(errors: BTreeSet<FieldError>) -> Vec<FieldError> {
    let hidden = errors.len().saturating_sub(MAX_FIELD_ERRORS);
    let mut errors: Vec<_> = errors.into_iter().take(MAX_FIELD_ERRORS).collect();
    if hidden > 0 {
        errors.push(FieldError::new("", hidden_errors(hidden)));
    }
    errors
}

/// Counts the errors a capped refusal leaves out.
fn hidden_errors(hidden: usize) -> String {
    if hidden == 1 {
        "1 more error is not shown. Fix these and call again.".to_owned()
    } else {
        format!("{hidden} more errors are not shown. Fix these and call again.")
    }
}

fn collect(error: &ValidationError<'_>, errors: &mut BTreeSet<FieldError>) {
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
                let path = if key.len() <= MAX_ECHOED_NAME_BYTES {
                    child(&path, key)
                } else {
                    path.clone()
                };
                record(errors, path, "Unexpected field.");
            }
        }
        Kind::OneOfNotValid { context } | Kind::AnyOf { context } => {
            if let Some(branch) = context.iter().min_by_key(|branch| {
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
        Kind::UniqueItems => record(errors, path, "Remove the repeated entries."),
        Kind::Custom { message, .. } => record(errors, path, message.clone()),
        Kind::MaxLength { limit } => {
            record(errors, path, format!("Use at most {limit} characters."))
        }
        Kind::MinLength { limit } => {
            record(errors, path, format!("Use at least {limit} characters."))
        }
        Kind::Maximum { limit } => record(errors, path, format!("Use at most {limit}.")),
        Kind::Minimum { limit } => record(errors, path, format!("Use at least {limit}.")),
        Kind::Pattern { pattern } => record(errors, path, pattern_message(pattern)),
        Kind::Type { .. } => record(errors, path, "Value has the wrong JSON type."),
        _ => record(errors, path, "Value does not match the tool schema."),
    }
}

fn pattern_message(pattern: &str) -> &'static str {
    match pattern {
        FINDING_ID_PATTERN => "Use a finding ID such as F-01.",
        GIT_SHA_PATTERN => "Use the full 40- or 64-character SHA of a commit.",
        _ => "Value does not match the required format.",
    }
}

fn child(path: &str, key: &str) -> String {
    format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"))
}

fn record(errors: &mut BTreeSet<FieldError>, path: String, message: impl Into<String>) {
    errors.insert(FieldError::new(path, message));
}
