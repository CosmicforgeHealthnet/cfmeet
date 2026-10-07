//! `cf-service-keygen`: writes private keys only to a new 0600 file, prints only the
//! public JWK, and the printed JWK matches the written key.

use std::process::Command;

use cf_service_auth::{KeyId, PublicJwk, ServiceSigningKey};

fn keygen(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_cf-service-keygen"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn generates_a_private_key_file_and_prints_only_the_public_jwk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("calling-staging.pem");
    let out = keygen(&[
        "generate",
        "--kid",
        "calling-staging-2026-10",
        "--out",
        path.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    let jwk: PublicJwk = serde_json::from_str(&stdout).unwrap();
    assert_eq!(jwk.kid.as_str(), "calling-staging-2026-10");
    assert_eq!((jwk.kty.as_str(), jwk.crv.as_str()), ("OKP", "Ed25519"));

    let pem = std::fs::read_to_string(&path).unwrap();
    assert!(pem.starts_with("-----BEGIN PRIVATE KEY-----"));
    let body = pem.lines().nth(1).unwrap();
    assert!(
        !stdout.contains(body) && !stderr.contains(body),
        "private key printed"
    );
    let loaded =
        ServiceSigningKey::from_pkcs8_pem(KeyId::parse("calling-staging-2026-10").unwrap(), &pem)
            .unwrap();
    assert_eq!(loaded.public_jwk(), jwk);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    // Never overwrites an existing key.
    let again = keygen(&["generate", "--kid", "x", "--out", path.to_str().unwrap()]);
    assert!(!again.status.success());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), pem);

    // public-jwk reproduces the same public key.
    let public = keygen(&[
        "public-jwk",
        "--kid",
        "calling-staging-2026-10",
        "--key",
        path.to_str().unwrap(),
    ]);
    assert!(public.status.success());
    assert_eq!(
        serde_json::from_slice::<PublicJwk>(&public.stdout).unwrap(),
        jwk
    );
}

#[test]
fn rejects_invalid_arguments() {
    assert!(!keygen(&[]).status.success());
    assert!(
        !keygen(&["generate", "--kid", "bad kid", "--out", "/tmp/x"])
            .status
            .success()
    );
    let missing = keygen(&["public-jwk", "--kid", "k", "--key", "/nonexistent.pem"]);
    assert!(!missing.status.success());
}
