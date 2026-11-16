//! Structured request-validation errors (F1).
//!
//! `validator` reports problems under the **Rust** field names of the request struct
//! and its `Display` output is one opaque string (struct-level checks appear as
//! `__all__: ...`). Clients need to know which **public** JSON field is wrong, so a
//! failed [`Validate`] is turned into `400 VALIDATION_ERROR` whose `error.details` lists
//! one [`ValidationIssue`] per problem, while `error.message` keeps the historical
//! human-readable `field: message` lines for older clients.
//!
//! Public names come from the request's OpenAPI schema ([`ToSchema`]), which already
//! applies the struct's serde naming: each Rust field name is matched against the
//! schema's properties under every serde `rename_all` convention (snake, camel, Pascal,
//! kebab, SCREAMING, lower/UPPER). Nested and list errors get dotted / indexed paths
//! (`participants[2].email`); below a `$ref` schema the parent's convention applies. A
//! per-field `#[serde(rename = "...")]` that is not a case conversion of the Rust name
//! cannot be recovered this way (a warning is logged and the convention is used): give
//! such a field a matching Rust name. A struct-level (`#[validate(schema(...))]`) check
//! that concerns one field names it with the [`FIELD_PARAM`] param (its Rust or public
//! name); otherwise it is reported with `field: null` (or the enclosing path when
//! nested).

use std::collections::BTreeMap;

use serde::Serialize;
use utoipa::openapi::schema::Schema;
use utoipa::openapi::RefOr;
use utoipa::ToSchema;
use validator::{Validate, ValidationError, ValidationErrors, ValidationErrorsKind};

use crate::error::{ApiError, ApiResult};

/// `ValidationError` param a struct-level check sets to the **Rust** name of the field
/// the problem belongs to (e.g. `confirm_password` for a password mismatch).
pub const FIELD_PARAM: &str = "field";

/// Message used when a validation rule carries no message of its own (rule params are
/// never echoed: they can contain the submitted value, e.g. a password).
const DEFAULT_MESSAGE: &str = "Invalid value";

/// One request-validation problem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ValidationIssue {
    /// Public JSON path of the offending field, as sent by the client (honours the
    /// request's serde naming), e.g. `password`, `displayName`, `items[0].email`.
    /// `null` when the problem concerns the request as a whole.
    #[schema(example = "password", required = true)]
    pub field: Option<String>,
    /// Stable, machine-readable rule code, e.g. `length`, `email`, `regex`,
    /// `password_strength`, `passwords_mismatch`.
    #[schema(example = "password_strength")]
    pub code: String,
    /// Safe, human-readable message for this field.
    #[schema(example = "Password must be between 8 and 72 characters")]
    pub message: String,
}

/// Validate a request body, mapping failures to a structured `400 VALIDATION_ERROR`.
pub trait ValidateRequest {
    fn validate_request(&self) -> ApiResult<()>;
}

impl<T> ValidateRequest for T
where
    T: Validate + for<'s> ToSchema<'s>,
{
    fn validate_request(&self) -> ApiResult<()> {
        self.validate().map_err(validation_failed::<T>)
    }
}

/// `400 VALIDATION_ERROR` for `errors` raised by validating a `T`.
pub fn validation_failed<T>(errors: ValidationErrors) -> ApiError
where
    T: for<'s> ToSchema<'s>,
{
    let (_, schema) = T::schema();
    let details = issues(&errors, inline(&schema));
    ApiError::InvalidFields {
        message: summary(&details),
        details,
    }
}

/// The structured issues of `errors`, using `schema` (the request's OpenAPI schema) for
/// public field names. Sorted by path for a deterministic response.
pub fn issues(errors: &ValidationErrors, schema: Option<&Schema>) -> Vec<ValidationIssue> {
    let mut out = Vec::new();
    collect(errors, &Scope::new(schema, Case::Snake), None, &mut out);
    out.sort_by(|a, b| a.field.cmp(&b.field));
    out
}

/// The historical `field: message[, message]` lines (one per field) with public names.
fn summary(details: &[ValidationIssue]) -> String {
    let mut lines: Vec<(Option<&str>, Vec<&str>)> = Vec::new();
    for issue in details {
        let field = issue.field.as_deref();
        match lines.iter_mut().find(|(f, _)| *f == field) {
            Some((_, messages)) => messages.push(&issue.message),
            None => lines.push((field, vec![&issue.message])),
        }
    }
    if lines.is_empty() {
        return "Validation failed".to_string();
    }
    lines
        .into_iter()
        .map(|(field, messages)| match field {
            Some(f) => format!("{f}: {}", messages.join(", ")),
            None => messages.join(", "),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn collect(
    errors: &ValidationErrors,
    scope: &Scope<'_>,
    prefix: Option<&str>,
    out: &mut Vec<ValidationIssue>,
) {
    for (key, kind) in errors.errors() {
        if key == "__all__" {
            if let ValidationErrorsKind::Field(errs) = kind {
                for e in errs {
                    let field = e
                        .params
                        .get(FIELD_PARAM)
                        .and_then(|v| v.as_str())
                        .map(|rust| join(prefix, &scope.resolve(rust).0))
                        .or_else(|| prefix.map(str::to_string));
                    out.push(issue(field, e));
                }
            }
            continue;
        }
        let (name, child) = scope.resolve(key);
        let path = join(prefix, &name);
        match kind {
            ValidationErrorsKind::Field(errs) => {
                out.extend(errs.iter().map(|e| issue(Some(path.clone()), e)));
            }
            ValidationErrorsKind::Struct(inner) => {
                collect(inner, &Scope::new(child, scope.case), Some(&path), out);
            }
            ValidationErrorsKind::List(items) => {
                let item_schema = child.and_then(array_items);
                for (index, inner) in items {
                    let item_path = format!("{path}[{index}]");
                    collect(
                        inner,
                        &Scope::new(item_schema, scope.case),
                        Some(&item_path),
                        out,
                    );
                }
            }
        }
    }
}

fn issue(field: Option<String>, e: &ValidationError) -> ValidationIssue {
    ValidationIssue {
        field,
        code: e.code.to_string(),
        message: e.message.as_deref().unwrap_or(DEFAULT_MESSAGE).to_string(),
    }
}

fn join(prefix: Option<&str>, name: &str) -> String {
    match prefix {
        Some(p) => format!("{p}.{name}"),
        None => name.to_string(),
    }
}

/// The schema itself when it is inline (a `$ref` cannot be followed without the
/// OpenAPI components; names below it then follow the inherited convention).
fn inline(schema: &RefOr<Schema>) -> Option<&Schema> {
    match schema {
        RefOr::T(s) => Some(s),
        RefOr::Ref(_) => None,
    }
}

fn array_items(schema: &Schema) -> Option<&Schema> {
    match schema {
        Schema::Array(a) => inline(&a.items),
        _ => None,
    }
}

/// Serde naming conventions (`rename_all` values) tried when mapping a Rust field name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Case {
    Snake,
    Camel,
    Pascal,
    Kebab,
    ScreamingSnake,
    ScreamingKebab,
    Lower,
    Upper,
}

const CASES: [Case; 8] = [
    Case::Snake,
    Case::Camel,
    Case::Pascal,
    Case::Kebab,
    Case::ScreamingSnake,
    Case::ScreamingKebab,
    Case::Lower,
    Case::Upper,
];

impl Case {
    /// Rename a Rust (snake_case) field name the way serde's `rename_all` does.
    fn apply(self, rust: &str) -> String {
        let words = rust.split('_').filter(|w| !w.is_empty());
        let cap = |w: &str| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect::<String>())
                .unwrap_or_default()
        };
        match self {
            Case::Snake => rust.to_string(),
            Case::Camel => words
                .enumerate()
                .map(|(i, w)| if i == 0 { w.to_string() } else { cap(w) })
                .collect(),
            Case::Pascal => words.map(cap).collect(),
            Case::Kebab => rust.replace('_', "-"),
            Case::ScreamingSnake => rust.to_uppercase(),
            Case::ScreamingKebab => rust.replace('_', "-").to_uppercase(),
            Case::Lower => rust.replace('_', ""),
            Case::Upper => rust.replace('_', "").to_uppercase(),
        }
    }

    /// Best guess of the convention behind a set of public property names: the most
    /// common signal (an individually renamed field does not outvote `rename_all`);
    /// names without a signal (`email`, `user_id`) do not vote.
    fn infer<'a>(names: impl Iterator<Item = &'a String>) -> Option<Case> {
        let mut votes: Vec<(Case, usize)> = Vec::new();
        for n in names {
            let case = if n.contains('-') {
                if n.chars().any(|c| c.is_ascii_lowercase()) {
                    Case::Kebab
                } else {
                    Case::ScreamingKebab
                }
            } else if n.starts_with(|c: char| c.is_ascii_uppercase()) {
                if n.chars().any(|c| c.is_ascii_lowercase()) {
                    Case::Pascal
                } else {
                    continue; // "ID": ambiguous
                }
            } else if n.chars().any(|c| c.is_ascii_uppercase()) {
                Case::Camel
            } else {
                continue; // all lower-case: snake or single word, no signal
            };
            match votes.iter_mut().find(|(c, _)| *c == case) {
                Some((_, count)) => *count += 1,
                None => votes.push((case, 1)),
            }
        }
        // Ties keep the first convention seen.
        votes
            .iter()
            .fold(None::<(Case, usize)>, |best, &(c, n)| match best {
                Some((_, m)) if m >= n => best,
                _ => Some((c, n)),
            })
            .map(|(c, _)| c)
    }
}

/// Public property names of one (inline) object schema.
struct Scope<'a> {
    properties: Option<&'a BTreeMap<String, RefOr<Schema>>>,
    case: Case,
}

impl<'a> Scope<'a> {
    fn new(schema: Option<&'a Schema>, inherited: Case) -> Self {
        let properties = schema.and_then(|s| match s {
            Schema::Object(o) => Some(&o.properties),
            _ => None,
        });
        let case = properties
            .and_then(|p| Case::infer(p.keys()))
            .unwrap_or(inherited);
        Self { properties, case }
    }

    /// Public name of the Rust field `rust` and its inline schema, when known.
    fn resolve(&self, rust: &str) -> (String, Option<&'a Schema>) {
        let rust = rust.strip_prefix("r#").unwrap_or(rust);
        if let Some(props) = self.properties {
            for case in CASES {
                let candidate = case.apply(rust);
                if let Some(schema) = props.get(&candidate) {
                    return (candidate, inline(schema).and_then(object_or_array));
                }
            }
            tracing::warn!(
                field = rust,
                "validated field has no matching public schema property; \
                 reporting it under the struct's naming convention"
            );
        }
        (self.case.apply(rust), None)
    }
}

/// `Option<T>` fields are inline objects / arrays; anything else carries no names.
fn object_or_array(schema: &Schema) -> Option<&Schema> {
    match schema {
        Schema::Object(_) | Schema::Array(_) => Some(schema),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, Validate, ToSchema)]
    #[serde(rename_all = "camelCase")]
    struct Item {
        #[validate(email(message = "Invalid email address"))]
        contact_email: String,
    }

    #[derive(Debug, Deserialize, Validate, ToSchema)]
    #[serde(rename_all = "camelCase")]
    #[validate(schema(function = "check_window"))]
    struct CamelRequest {
        #[validate(length(min = 1, message = "Display name is required"))]
        display_name: String,
        #[serde(rename = "TimeZone")]
        #[validate(length(min = 1, message = "Timezone is required"))]
        time_zone: String,
        #[validate(length(max = 1, message = "At most one tag"))]
        tags: Vec<String>,
        #[validate(nested)]
        items: Vec<Item>,
        #[validate(nested)]
        primary: Item,
        starts_at: i64,
        ends_at: i64,
    }

    fn check_window(r: &CamelRequest) -> Result<(), ValidationError> {
        if r.ends_at <= r.starts_at {
            let mut e =
                ValidationError::new("window").with_message("End must be after start".into());
            e.add_param(FIELD_PARAM.into(), &"ends_at");
            return Err(e);
        }
        Ok(())
    }

    #[derive(Debug, Deserialize, Validate, ToSchema)]
    #[validate(schema(function = "whole"))]
    struct SnakeRequest {
        #[validate(length(min = 3, message = "Too short"))]
        user_name: String,
    }

    fn whole(_: &SnakeRequest) -> Result<(), ValidationError> {
        Err(ValidationError::new("whole").with_message("Not allowed".into()))
    }

    fn details(e: ApiError) -> (String, Vec<ValidationIssue>) {
        match e {
            ApiError::InvalidFields { message, details } => (message, details),
            other => panic!("unexpected {other:?}"),
        }
    }

    fn fields(d: &[ValidationIssue]) -> Vec<Option<&str>> {
        d.iter().map(|i| i.field.as_deref()).collect()
    }

    #[test]
    fn rust_names_become_public_camel_case_and_renamed_paths() {
        let req = CamelRequest {
            display_name: String::new(),
            time_zone: String::new(),
            tags: vec!["a".into(), "b".into()],
            items: vec![
                Item {
                    contact_email: "ok@example.com".into(),
                },
                Item {
                    contact_email: "nope".into(),
                },
            ],
            primary: Item {
                contact_email: "bad".into(),
            },
            starts_at: 2,
            ends_at: 1,
        };
        let (message, d) = details(req.validate_request().unwrap_err());
        assert_eq!(
            fields(&d),
            vec![
                Some("TimeZone"),
                Some("displayName"),
                Some("items[1].contactEmail"),
                Some("primary.contactEmail"),
                Some("tags"),
            ]
        );
        assert_eq!(d[1].code, "length");
        assert_eq!(d[1].message, "Display name is required");
        assert_eq!(d[2].code, "email");
        assert!(message.contains("displayName: Display name is required"));
        assert!(message.contains("items[1].contactEmail: Invalid email address"));
        assert!(!message.contains("__all__") && !message.contains("display_name"));
    }

    #[test]
    fn struct_level_errors_name_their_field_or_the_whole_request() {
        let req = CamelRequest {
            display_name: "Ada".into(),
            time_zone: "UTC".into(),
            tags: vec![],
            items: vec![],
            primary: Item {
                contact_email: "a@example.com".into(),
            },
            starts_at: 2,
            ends_at: 1,
        };
        let (message, d) = details(req.validate_request().unwrap_err());
        assert_eq!(
            d,
            vec![ValidationIssue {
                field: Some("endsAt".into()),
                code: "window".into(),
                message: "End must be after start".into(),
            }]
        );
        assert_eq!(message, "endsAt: End must be after start");

        let (message, d) = details(
            SnakeRequest {
                user_name: "abcd".into(),
            }
            .validate_request()
            .unwrap_err(),
        );
        assert_eq!(fields(&d), vec![None]);
        assert_eq!(message, "Not allowed");

        let (message, d) = details(
            SnakeRequest {
                user_name: "a".into(),
            }
            .validate_request()
            .unwrap_err(),
        );
        assert_eq!(fields(&d), vec![Some("user_name")], "snake_case stays");
        assert_eq!(message, "user_name: Too short");
    }

    #[test]
    fn case_conversion_matches_serde() {
        assert_eq!(Case::Camel.apply("confirm_password"), "confirmPassword");
        assert_eq!(Case::Pascal.apply("confirm_password"), "ConfirmPassword");
        assert_eq!(Case::Kebab.apply("confirm_password"), "confirm-password");
        assert_eq!(
            Case::ScreamingSnake.apply("confirm_password"),
            "CONFIRM_PASSWORD"
        );
        assert_eq!(Case::Camel.apply("email"), "email");
    }

    #[test]
    fn rule_params_are_never_echoed() {
        let mut errors = ValidationErrors::new();
        let mut e = ValidationError::new("custom");
        e.add_param("value".into(), &"hunter2");
        errors.add("password", e);
        let d = issues(&errors, None);
        assert_eq!(d[0].message, DEFAULT_MESSAGE);
        assert!(!format!("{d:?}").contains("hunter2"));
    }
}
