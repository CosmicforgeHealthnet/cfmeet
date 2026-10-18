//! Secret values from the environment or from a mounted secret file (Part H §74.11).
//!
//! Every Platform Access secret `NAME` may instead be provided as `NAME_FILE`: the path of a file
//! (e.g. a Kubernetes / Docker secret mount) whose trimmed contents are the value.
//!
//! * `NAME_FILE` is preferred; `NAME` (inline) is still accepted where it was before, so
//!   local development is unaffected.
//! * Both set → refused (ambiguous: which one is authoritative is never guessed).
//! * `NAME_FILE` set but unreadable / empty → refused. There is no fallback to the inline
//!   value or to any default.
//! * Outside the `local` and `test` runtime environments (`CF_RUNTIME_ENVIRONMENT`; unset
//!   counts as strict) a secret file readable by group or others is refused.
//!
//! Errors name the variable and the reason; they never echo the value or the file's
//! contents (the path is configuration, not a secret, but it is not echoed either).

use secrecy::SecretString;

use super::ConfigError;

/// Largest secret file accepted (a key or pepper is far smaller).
const MAX_SECRET_FILE_BYTES: u64 = 16 * 1024;

/// Whether `CF_RUNTIME_ENVIRONMENT` allows permissive secret-file modes (`local`, `test`).
/// Anything else, including an unset value, is strict.
pub fn permissive_runtime(get: &impl Fn(&str) -> Option<String>) -> bool {
    matches!(
        get("CF_RUNTIME_ENVIRONMENT").as_deref().map(str::trim),
        Some("local") | Some("test")
    )
}

/// Reads the secret `name` (inline) or `file_var` (`<name>_FILE`, a path).
///
/// `Ok(None)`: neither is set (the caller decides whether that is an error).
pub fn read_secret(
    get: &impl Fn(&str) -> Option<String>,
    name: &'static str,
    file_var: &'static str,
) -> Result<Option<SecretString>, ConfigError> {
    let inline = get(name).filter(|v| !v.trim().is_empty());
    let path = get(file_var)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    match (inline, path) {
        (Some(_), Some(_)) => Err(ConfigError::invalid(
            name,
            format!("both {name} and {file_var} are set; configure exactly one"),
        )),
        (Some(value), None) => Ok(Some(SecretString::from(value))),
        (None, Some(path)) => read_file(file_var, &path, permissive_runtime(get)).map(Some),
        (None, None) => Ok(None),
    }
}

/// [`read_secret`] for a required secret: missing is [`ConfigError::Missing`].
pub fn required_secret(
    get: &impl Fn(&str) -> Option<String>,
    name: &'static str,
    file_var: &'static str,
) -> Result<SecretString, ConfigError> {
    read_secret(get, name, file_var)?.ok_or(ConfigError::Missing { var: name })
}

/// A lookup in which every listed secret `name` resolves through [`read_secret`] (inline
/// or `<name>_FILE`) and every other key through `get` — so existing `from_lookup`
/// parsers transparently accept the file form. Resolution errors (ambiguous, unreadable,
/// permissive file) are returned up front, before any parser runs.
pub fn with_secret_files<'a>(
    get: &'a impl Fn(&str) -> Option<String>,
    secrets: &[(&'static str, &'static str)],
) -> Result<impl Fn(&str) -> Option<String> + 'a, ConfigError> {
    use secrecy::ExposeSecret;
    let mut resolved = std::collections::HashMap::with_capacity(secrets.len() * 2);
    for (name, file_var) in secrets {
        let value = read_secret(get, name, file_var)?.map(|s| s.expose_secret().to_string());
        resolved.insert(*name, value);
        // The parsers never see the path.
        resolved.insert(*file_var, None);
    }
    Ok(move |key: &str| match resolved.get(key) {
        Some(value) => value.clone(),
        None => get(key),
    })
}

fn read_file(var: &'static str, path: &str, permissive: bool) -> Result<SecretString, ConfigError> {
    let meta = std::fs::metadata(path)
        .map_err(|_| ConfigError::invalid(var, "the secret file cannot be read".to_string()))?;
    if !meta.is_file() {
        return Err(ConfigError::invalid(
            var,
            "the secret file is not a regular file".to_string(),
        ));
    }
    if meta.len() > MAX_SECRET_FILE_BYTES {
        return Err(ConfigError::invalid(
            var,
            "the secret file is too large".to_string(),
        ));
    }
    #[cfg(unix)]
    if !permissive {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(ConfigError::invalid(
                var,
                "the secret file must not be readable by group or others (chmod 0400 / 0600)"
                    .to_string(),
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = permissive;
    let raw = std::fs::read(path)
        .map_err(|_| ConfigError::invalid(var, "the secret file cannot be read".to_string()))?;
    let text = String::from_utf8(raw)
        .map_err(|_| ConfigError::invalid(var, "the secret file is not UTF-8".to_string()))?;
    let value = text.trim();
    if value.is_empty() {
        return Err(ConfigError::invalid(
            var,
            "the secret file is empty".to_string(),
        ));
    }
    Ok(SecretString::from(value.to_string()))
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashMap;

    use secrecy::ExposeSecret;

    use super::*;

    /// A secret file under the system temp directory with `mode`, removed on drop.
    pub(crate) struct TempSecret(pub std::path::PathBuf);

    impl TempSecret {
        pub(crate) fn new(contents: &str, mode: u32) -> Self {
            let path = std::env::temp_dir().join(format!(
                "platform-access-secret-test-{}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::write(&path, contents).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            }
            #[cfg(not(unix))]
            let _ = mode;
            Self(path)
        }

        pub(crate) fn path(&self) -> String {
            self.0.to_string_lossy().into_owned()
        }
    }

    impl Drop for TempSecret {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn lookup(pairs: &[(&str, String)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        move |k| map.get(k).cloned()
    }

    const NAME: &str = "CALLING_TEST_SECRET";
    const FILE: &str = "CALLING_TEST_SECRET_FILE";
    const VALUE: &str = "file-secret-0123456789abcdefghijklmnopqrstuvwxyz";

    #[test]
    fn the_file_is_read_trimmed_and_preferred_over_nothing() {
        let file = TempSecret::new(&format!("  {VALUE}\n"), 0o600);
        let get = lookup(&[
            ("CF_RUNTIME_ENVIRONMENT", "production".into()),
            (FILE, file.path()),
        ]);
        let value = read_secret(&get, NAME, FILE).unwrap().unwrap();
        assert_eq!(value.expose_secret(), VALUE);
        // The wrapper hands parsers the value under NAME and never the path.
        let wrapped = with_secret_files(&get, &[(NAME, FILE)]).unwrap();
        assert_eq!(wrapped(NAME).as_deref(), Some(VALUE));
        assert_eq!(wrapped(FILE), None);
        assert_eq!(
            wrapped("CF_RUNTIME_ENVIRONMENT").as_deref(),
            Some("production")
        );
    }

    #[test]
    fn inline_values_still_work_and_nothing_set_is_none() {
        let get = lookup(&[(NAME, VALUE.into())]);
        assert_eq!(
            read_secret(&get, NAME, FILE)
                .unwrap()
                .unwrap()
                .expose_secret(),
            VALUE
        );
        let empty = lookup(&[]);
        assert!(read_secret(&empty, NAME, FILE).unwrap().is_none());
        assert!(matches!(
            required_secret(&empty, NAME, FILE),
            Err(ConfigError::Missing { var: NAME })
        ));
    }

    #[test]
    fn a_missing_or_empty_file_is_an_error_never_a_fallback() {
        let missing = std::env::temp_dir().join("platform-access-secret-test-does-not-exist");
        for get in [
            lookup(&[(FILE, missing.to_string_lossy().into_owned())]),
            lookup(&[
                ("CF_RUNTIME_ENVIRONMENT", "local".into()),
                (FILE, missing.to_string_lossy().into_owned()),
            ]),
        ] {
            let err = read_secret(&get, NAME, FILE).unwrap_err();
            assert!(
                matches!(err, ConfigError::Invalid { var: FILE, .. }),
                "{err}"
            );
            assert!(!err.to_string().contains("does-not-exist"), "path echoed");
        }
        let empty = TempSecret::new(" \n", 0o600);
        let get = lookup(&[(FILE, empty.path())]);
        assert!(read_secret(&get, NAME, FILE).is_err());
        // A directory is not a secret file.
        let get = lookup(&[(FILE, std::env::temp_dir().to_string_lossy().into_owned())]);
        assert!(read_secret(&get, NAME, FILE).is_err());
    }

    #[test]
    fn both_inline_and_file_are_refused_as_ambiguous() {
        let file = TempSecret::new(VALUE, 0o600);
        let get = lookup(&[
            (NAME, "inline-value-zyxwvutsrqponmlkjihgfedcba".into()),
            (FILE, file.path()),
        ]);
        let err = read_secret(&get, NAME, FILE).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { var: NAME, .. }));
        assert!(!err.to_string().contains(VALUE));
        assert!(!err.to_string().contains("inline-value"));
        assert!(with_secret_files(&get, &[(NAME, FILE)]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn permissive_files_are_refused_outside_local_and_test() {
        let readable = TempSecret::new(VALUE, 0o644);
        for runtime in [Some("production"), Some("staging"), None, Some("prod")] {
            let mut pairs = vec![(FILE, readable.path())];
            if let Some(r) = runtime {
                pairs.push(("CF_RUNTIME_ENVIRONMENT", r.to_string()));
            }
            let err = read_secret(&lookup(&pairs), NAME, FILE).unwrap_err();
            assert!(
                matches!(err, ConfigError::Invalid { var: FILE, .. }),
                "{runtime:?}: {err}"
            );
            assert!(!err.to_string().contains(VALUE));
        }
        let group = TempSecret::new(VALUE, 0o640);
        let get = lookup(&[
            ("CF_RUNTIME_ENVIRONMENT", "production".into()),
            (FILE, group.path()),
        ]);
        assert!(read_secret(&get, NAME, FILE).is_err());
        // Local development and tests may use permissive files.
        for runtime in ["local", "test"] {
            let get = lookup(&[
                ("CF_RUNTIME_ENVIRONMENT", runtime.into()),
                (FILE, readable.path()),
            ]);
            assert_eq!(
                read_secret(&get, NAME, FILE)
                    .unwrap()
                    .unwrap()
                    .expose_secret(),
                VALUE
            );
        }
        // Owner-only files are accepted in production.
        for mode in [0o400, 0o600] {
            let strict = TempSecret::new(VALUE, mode);
            let get = lookup(&[
                ("CF_RUNTIME_ENVIRONMENT", "production".into()),
                (FILE, strict.path()),
            ]);
            assert!(read_secret(&get, NAME, FILE).unwrap().is_some());
        }
    }
}
