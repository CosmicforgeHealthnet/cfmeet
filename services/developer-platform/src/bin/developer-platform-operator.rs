//! Operator review tool for production-access requests (final completion, Section 6).
//!
//! The operator console of the production-access workflow: it signs every call as
//! `cfmeet.operator-console` (the only identity `developer-platform.administer` may be
//! granted to), names the human operator (`x-cf-operator`) and, for decisions, the
//! operator's CF Meet user id (`x-cf-operator-user-id`, self-approval check). It runs on
//! the production host from the Developer Platform image (coordinated release operation
//! `developer-access-review`), never in a browser, and prints the Developer Platform's
//! JSON answer (request metadata only; no credential, token or secret).
//!
//! ```text
//! developer-platform-operator list [STATE]
//! developer-platform-operator get <REQUEST_ID>
//! developer-platform-operator review <REQUEST_ID>
//! developer-platform-operator approve <REQUEST_ID> [VALID_DAYS]
//! developer-platform-operator reject <REQUEST_ID> <REASON_CODE>
//! developer-platform-operator apps list [STATUS]
//! developer-platform-operator apps get <APP_ID>
//! developer-platform-operator apps review|approve|publish <APP_ID>
//! developer-platform-operator apps reject <APP_ID> <REASON_CODE>
//! developer-platform-operator apps suspend <APP_ID> [REASON_CODE]
//! ```
//!
//! ADR 0065 app review (`apps …`, flag `DEVELOPER_PLATFORM_APPS_ENABLED`): review /
//! approve / reject / publish need a reviewer named in `DEVELOPER_PLATFORM_APP_REVIEWERS`
//! and `CF_OPERATOR_USER_ID` (no self-review); suspend needs neither.
//!
//! Environment: `CF_RUNTIME_ENVIRONMENT`, `CF_SERVICE_AUTH_KEY_ID`,
//! `CF_SERVICE_AUTH_PRIVATE_KEY_FILE` (the operator-console signing key),
//! `CF_DEVELOPER_PLATFORM_BASE_URL`, `CF_OPERATOR`, `CF_OPERATOR_USER_ID` (decisions).

use std::{path::PathBuf, process::ExitCode, sync::Arc, time::Duration};

use cf_service_auth::{
    clock::SystemClock,
    identity::{RuntimeEnvironment, ServiceId},
    ids::KeyId,
    keys::ServiceSigningKey,
    signer::{OutboundServiceRequest, ServiceTokenSigner, TokenTtl},
};
use serde_json::{json, Value};
use uuid::Uuid;

const OPERATOR_CONSOLE: &str = "cfmeet.operator-console";
const DEVELOPER_PLATFORM: &str = "cfmeet.developer-platform";
const BASE: &str = "/internal/v1/operator/production-access-requests";
const APPS: &str = "/internal/v1/operator/apps";

#[derive(Debug, PartialEq, Eq)]
enum Command {
    List(Option<String>),
    Get(Uuid),
    Review(Uuid),
    Approve(Uuid, Option<i64>),
    Reject(Uuid, String),
    /// ADR 0065: app review.
    Apps(AppCommand),
}

#[derive(Debug, PartialEq, Eq)]
enum AppCommand {
    List(Option<String>),
    Get(Uuid),
    /// review / approve / publish / reject / suspend, with the reason code of reject and
    /// suspend.
    Decide(Uuid, &'static str, Option<String>),
}

fn parse_apps(args: &[String]) -> Result<Command, String> {
    let id = |i: usize| -> Result<Uuid, String> {
        args.get(i)
            .and_then(|v| Uuid::parse_str(v).ok())
            .ok_or_else(|| "an app id (UUID) is required".to_string())
    };
    let usage = "usage: developer-platform-operator apps <list [STATUS]|get ID|review ID|approve ID|publish ID|reject ID REASON|suspend ID [REASON]>";
    let cmd = match args.first().map(String::as_str) {
        Some("list") => {
            let status = args.get(1).cloned();
            if status
                .as_deref()
                .is_some_and(|s| !s.bytes().all(|b| b.is_ascii_uppercase() || b == b'_'))
            {
                return Err("STATUS must be an upper-case status name".into());
            }
            AppCommand::List(status)
        }
        Some("get") => AppCommand::Get(id(1)?),
        Some(c @ ("review" | "approve" | "publish")) => {
            let c: &'static str = match c {
                "review" => "review",
                "approve" => "approve",
                _ => "publish",
            };
            AppCommand::Decide(id(1)?, c, None)
        }
        Some("reject") => {
            let reason = args.get(2).cloned().unwrap_or_default();
            if !reason_ok(&reason) {
                return Err("apps reject needs a REASON_CODE (e.g. MISLEADING_NAME)".into());
            }
            AppCommand::Decide(id(1)?, "reject", Some(reason))
        }
        Some("suspend") => {
            let reason = args.get(2).cloned();
            if reason.as_deref().is_some_and(|r| !reason_ok(r)) {
                return Err("REASON_CODE must be an upper-case code".into());
            }
            AppCommand::Decide(id(1)?, "suspend", reason)
        }
        _ => return Err(usage.into()),
    };
    Ok(Command::Apps(cmd))
}

fn reason_ok(r: &str) -> bool {
    !r.is_empty()
        && r.len() <= 64
        && r.bytes()
            .all(|b| b.is_ascii_uppercase() || b == b'_' || b.is_ascii_digit())
}

fn parse(args: &[String]) -> Result<Command, String> {
    let id = |i: usize| -> Result<Uuid, String> {
        args.get(i)
            .and_then(|v| Uuid::parse_str(v).ok())
            .ok_or_else(|| "a production-access request id (UUID) is required".to_string())
    };
    match args.first().map(String::as_str) {
        Some("list") => {
            let state = args.get(1).cloned();
            if state
                .as_deref()
                .is_some_and(|s| !s.bytes().all(|b| b.is_ascii_uppercase() || b == b'_'))
            {
                return Err("STATE must be an upper-case state name".into());
            }
            Ok(Command::List(state))
        }
        Some("apps") => parse_apps(&args[1..]),
        Some("get") => Ok(Command::Get(id(1)?)),
        Some("review") => Ok(Command::Review(id(1)?)),
        Some("approve") => {
            let days = args
                .get(2)
                .map(|d| {
                    d.parse::<i64>()
                        .ok()
                        .filter(|d| (1..=90).contains(d))
                        .ok_or("VALID_DAYS must be 1-90")
                })
                .transpose()?;
            Ok(Command::Approve(id(1)?, days))
        }
        Some("reject") => {
            // A rejection always states its reason (safe reason code).
            let reason = args.get(2).cloned().unwrap_or_default();
            if !reason_ok(&reason) {
                return Err("reject needs a REASON_CODE (e.g. NOT_ELIGIBLE, INCOMPLETE_INFORMATION)".into());
            }
            Ok(Command::Reject(id(1)?, reason))
        }
        _ => Err("usage: developer-platform-operator <list [STATE]|get ID|review ID|approve ID [DAYS]|reject ID REASON|apps …>".into()),
    }
}

fn request(cmd: &Command) -> (reqwest::Method, String, Option<Value>, bool) {
    use reqwest::Method;
    match cmd {
        Command::List(_) => (Method::GET, BASE.to_string(), None, false),
        Command::Get(id) => (Method::GET, format!("{BASE}/{id}"), None, false),
        Command::Review(id) => (
            Method::POST,
            format!("{BASE}/{id}/review"),
            Some(json!({})),
            false,
        ),
        Command::Approve(id, days) => {
            let mut body = json!({});
            if let Some(d) = days {
                body["approvalValidDays"] = json!(d);
            }
            (
                Method::POST,
                format!("{BASE}/{id}/approve"),
                Some(body),
                true,
            )
        }
        Command::Reject(id, reason) => (
            Method::POST,
            format!("{BASE}/{id}/reject"),
            Some(json!({ "reasonCode": reason })),
            true,
        ),
        Command::Apps(AppCommand::List(_)) => (Method::GET, APPS.to_string(), None, false),
        Command::Apps(AppCommand::Get(id)) => (Method::GET, format!("{APPS}/{id}"), None, false),
        Command::Apps(AppCommand::Decide(id, command, reason)) => {
            let mut body = json!({});
            if let Some(r) = reason {
                body["reasonCode"] = json!(r);
            }
            (
                Method::POST,
                format!("{APPS}/{id}/{command}"),
                Some(body),
                *command != "suspend",
            )
        }
    }
}

fn env(name: &str) -> Result<String, String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("{name} is required"))
}

async fn run(args: Vec<String>) -> Result<Value, String> {
    let cmd = parse(&args)?;
    let operator = env("CF_OPERATOR")?;
    if !developer_platform::control::operator::valid_operator(&operator) {
        return Err("CF_OPERATOR is not a valid operator name".into());
    }
    let runtime = RuntimeEnvironment::parse(&env("CF_RUNTIME_ENVIRONMENT")?)
        .map_err(|_| "CF_RUNTIME_ENVIRONMENT is invalid".to_string())?;
    let key_id = KeyId::parse(&env("CF_SERVICE_AUTH_KEY_ID")?)
        .map_err(|_| "CF_SERVICE_AUTH_KEY_ID is invalid".to_string())?;
    let key = ServiceSigningKey::from_pkcs8_pem_file(
        key_id,
        &PathBuf::from(env("CF_SERVICE_AUTH_PRIVATE_KEY_FILE")?),
    )
    .map_err(|_| "the operator-console signing key cannot be read".to_string())?;
    let base_url = env("CF_DEVELOPER_PLATFORM_BASE_URL")?;
    if !base_url.starts_with("https://") && !runtime.allows_insecure_transport() {
        return Err("CF_DEVELOPER_PLATFORM_BASE_URL must use https".into());
    }
    let signer = ServiceTokenSigner::new(
        ServiceId::parse(OPERATOR_CONSOLE).expect("canonical"),
        runtime,
        key,
        TokenTtl::seconds(60).expect("valid ttl"),
        Arc::new(SystemClock),
    );
    let (method, path, body, decision) = request(&cmd);
    let signed = signer
        .sign_request(
            &OutboundServiceRequest::new_operation(
                ServiceId::parse(DEVELOPER_PLATFORM).expect("canonical"),
            ),
            &method,
            &path,
        )
        .map_err(|_| "cannot sign the request".to_string())?;
    let mut headers = signed.headers();
    headers.insert("x-cf-operator", operator.parse().expect("validated"));
    if decision {
        let user = Uuid::parse_str(&env("CF_OPERATOR_USER_ID")?).map_err(|_| {
            "CF_OPERATOR_USER_ID must be the operator's CF Meet user id".to_string()
        })?;
        headers.insert(
            "x-cf-operator-user-id",
            user.to_string().parse().expect("uuid"),
        );
    }
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|_| "cannot build the HTTP client".to_string())?;
    let mut url = format!("{}{}", base_url.trim_end_matches('/'), path);
    if let Command::List(Some(state)) = &cmd {
        url.push_str(&format!("?state={state}"));
    }
    if let Command::Apps(AppCommand::List(Some(status))) = &cmd {
        url.push_str(&format!("?status={status}"));
    }
    let mut req = http.request(method, url).headers(headers);
    if let Some(b) = body {
        req = req.json(&b);
    }
    let response = req
        .send()
        .await
        .map_err(|_| "the Developer Platform is unreachable".to_string())?;
    let status = response.status().as_u16();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if !(200..300).contains(&status) {
        let code = body
            .pointer("/error/code")
            .or_else(|| body.get("code"))
            .and_then(Value::as_str)
            .unwrap_or("UNKNOWN");
        return Err(format!("HTTP {status} {code}"));
    }
    Ok(json!({ "status": status, "result": body }))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(args).await {
        Ok(v) => {
            println!("{v}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("developer-platform-operator: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn decisions_are_explicit() {
        let id = Uuid::new_v4().to_string();
        assert_eq!(parse(&args(&["list"])), Ok(Command::List(None)));
        assert!(parse(&args(&["list", "drop;"])).is_err());
        assert!(matches!(
            parse(&args(&["approve", &id, "30"])),
            Ok(Command::Approve(_, Some(30)))
        ));
        assert!(parse(&args(&["approve", &id, "365"])).is_err());
        assert!(
            parse(&args(&["reject", &id])).is_err(),
            "a rejection needs a reason"
        );
        assert!(parse(&args(&["reject", &id, "not eligible"])).is_err());
        assert!(matches!(
            parse(&args(&["reject", &id, "NOT_ELIGIBLE"])),
            Ok(Command::Reject(_, _))
        ));
        assert!(parse(&args(&["approve", "nope"])).is_err());
        assert!(parse(&args(&["auto-approve"])).is_err());
    }

    #[test]
    fn requests_target_the_operator_routes() {
        let id = Uuid::new_v4();
        let (m, p, b, d) = request(&Command::Reject(id, "NOT_ELIGIBLE".into()));
        assert_eq!(m, reqwest::Method::POST);
        assert_eq!(p, format!("{BASE}/{id}/reject"));
        assert_eq!(b.unwrap()["reasonCode"], "NOT_ELIGIBLE");
        assert!(d);
        assert!(!request(&Command::Get(id)).3);
    }

    #[test]
    fn app_review_commands_target_the_operator_app_routes() {
        let id = Uuid::new_v4();
        let s = id.to_string();
        assert_eq!(
            parse(&args(&["apps", "list", "SUBMITTED"])),
            Ok(Command::Apps(AppCommand::List(Some("SUBMITTED".into()))))
        );
        assert!(parse(&args(&["apps", "reject", &s])).is_err(), "a reason");
        assert!(parse(&args(&["apps", "suspend", &s, "bad reason"])).is_err());
        assert!(parse(&args(&["apps", "auto-publish", &s])).is_err());
        let (m, p, b, d) = request(&parse(&args(&["apps", "publish", &s])).unwrap());
        assert_eq!(
            (m, p.as_str()),
            (
                reqwest::Method::POST,
                format!("{APPS}/{id}/publish").as_str()
            )
        );
        assert!(d && b.is_some());
        let (_, p, b, d) =
            request(&parse(&args(&["apps", "reject", &s, "MISLEADING_NAME"])).unwrap());
        assert_eq!(p, format!("{APPS}/{id}/reject"));
        assert_eq!(b.unwrap()["reasonCode"], "MISLEADING_NAME");
        assert!(d);
        let (_, p, _, d) = request(&parse(&args(&["apps", "suspend", &s])).unwrap());
        assert_eq!(p, format!("{APPS}/{id}/suspend"));
        assert!(!d, "suspension needs no reviewer id");
        let (m, p, _, _) = request(&parse(&args(&["apps", "get", &s])).unwrap());
        assert_eq!((m, p), (reqwest::Method::GET, format!("{APPS}/{id}")));
    }
}
