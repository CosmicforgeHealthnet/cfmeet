//! §74.13 Contact Vault key rotation through the operator CLI (`privacy-relay-admin
//! contact-vault …`, the binary cargo builds for this test): encryption-key rotation
//! (generate → add-key → activate-key → reencrypt → retire-key) and fingerprint-key
//! rotation (add-key → backfill-fingerprints → activate-key → prune-fingerprints →
//! retire-key) against a real database, with the service redeployed on the files the
//! CLI wrote. Retirement is refused while a version is active or referenced (the file is
//! left byte-for-byte unchanged), `rotation-status` reports a referenced-but-unreadable
//! version, and no command ever prints key material or a number.

mod common;

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::Arc,
};

use cf_service_auth::ApplicationEnvironmentKind;
use common::{scalar, Consumer, Harness};
use privacy_relay::contact_vault::{
    crypto,
    keyring::{ContactKeyProvider, FileKeyring, KeyPurpose},
    repository, VaultKeys,
};
use serde_json::Value;
use uuid::Uuid;

const NUMBERS: [&str; 3] = ["+39 02 1234 5678", "+44 20 7946 0958", "+234 803 123 4567"];
const DIGITS: [&str; 4] = ["0212345678", "2079460958", "8031234567", "6502530000"];

/// A private scratch directory (keyring files live here), removed on drop.
struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("prl-admin-cli-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The operator CLI against the test database and the two keyring files.
struct Cli {
    dir: Dir,
    url: String,
    encryption: PathBuf,
    fingerprint: PathBuf,
    /// Everything any command printed (scanned for leaks at the end).
    transcript: RefCell<String>,
}

impl Cli {
    fn new(url: &str) -> Self {
        let dir = Dir::new();
        let encryption = dir.0.join("contact-keyring.json");
        let fingerprint = dir.0.join("fingerprint-keyring.json");
        Self {
            dir,
            url: url.to_string(),
            encryption,
            fingerprint,
            transcript: RefCell::new(String::new()),
        }
    }

    fn run_with(&self, encryption: &Path, args: &[&str]) -> Output {
        // A clean environment: only what the operator sets (no inherited .env / RUST_LOG).
        let out = Command::new(env!("CARGO_BIN_EXE_privacy-relay-admin"))
            .args(args)
            .current_dir(&self.dir.0)
            .env_clear()
            .env("DATABASE_URL", &self.url)
            .env("CF_RUNTIME_ENVIRONMENT", "test")
            .env("CONTACT_VAULT_KEYRING_FILE", encryption)
            .env("CONTACT_FINGERPRINT_KEYRING_FILE", &self.fingerprint)
            .output()
            .expect("run privacy-relay-admin");
        let mut t = self.transcript.borrow_mut();
        t.push_str(&String::from_utf8_lossy(&out.stdout));
        t.push_str(&String::from_utf8_lossy(&out.stderr));
        out
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_with(&self.encryption, args)
    }

    /// A successful command; returns its stdout.
    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{args:?}: {:?} {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// A command refused with exit code 1 that leaves `file` byte-for-byte unchanged;
    /// returns its stderr.
    fn refused(&self, args: &[&str], file: &Path) -> String {
        let before = std::fs::read(file).unwrap();
        let out = self.run(args);
        assert_eq!(
            out.status.code(),
            Some(1),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            std::fs::read(file).unwrap(),
            before,
            "{args:?} changed the keyring"
        );
        String::from_utf8_lossy(&out.stderr).into_owned()
    }

    fn path(p: &Path) -> &str {
        p.to_str().unwrap()
    }

    fn backups(&self) -> usize {
        std::fs::read_dir(&self.dir.0)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".bak-")
            })
            .count()
    }

    /// The keys the service would load from the two files now (a redeploy).
    fn load(&self) -> VaultKeys {
        VaultKeys::new(
            Arc::new(FileKeyring::from_file(&self.encryption, KeyPurpose::Encryption).unwrap()),
            Arc::new(FileKeyring::from_file(&self.fingerprint, KeyPurpose::Fingerprint).unwrap()),
        )
        .unwrap()
    }

    /// Every base64 key any version of `file` held so far (to prove none is printed).
    fn key_material(file: &Path, seen: &mut Vec<String>) {
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
        for k in doc["keys"].as_array().unwrap() {
            let key = k["key"].as_str().unwrap().to_string();
            if !seen.contains(&key) {
                seen.push(key);
            }
        }
    }

    /// No command printed key material or any contact number.
    fn assert_nothing_leaked(&self, keys: &[String]) {
        let t = self.transcript.borrow();
        assert!(!t.is_empty());
        for k in keys {
            assert!(!t.contains(k.as_str()), "key material printed");
        }
        for d in DIGITS {
            assert!(!t.contains(d), "a number was printed");
        }
    }
}

fn active(file: &Path, purpose: KeyPurpose) -> (String, Vec<String>) {
    let ring = FileKeyring::from_file(file, purpose).unwrap();
    let mut versions: Vec<String> = ring
        .versions()
        .iter()
        .map(|v| v.as_str().to_string())
        .collect();
    versions.sort();
    (ring.current_key().0.as_str().to_string(), versions)
}

fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Generates both keyrings with the CLI and deploys the service on them.
fn generate(cli: &Cli, encryption_version: &str, fingerprint_version: &str) {
    for (purpose, version, file) in [
        ("encryption", encryption_version, &cli.encryption),
        ("fingerprint", fingerprint_version, &cli.fingerprint),
    ] {
        cli.ok(&[
            "contact-vault",
            "generate-keyring",
            "--purpose",
            purpose,
            "--version",
            version,
            "--out",
            Cli::path(file),
        ]);
        assert_eq!(mode(file), 0o600);
    }
    // generate-keyring never overwrites.
    let out = cli.run(&[
        "contact-vault",
        "generate-keyring",
        "--purpose",
        "encryption",
        "--version",
        "ck-other",
        "--out",
        Cli::path(&cli.encryption),
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        active(&cli.encryption, KeyPurpose::Encryption).0,
        encryption_version
    );
}

async fn contacts(h: &Harness, c: &Consumer) -> Vec<Value> {
    let mut out = Vec::new();
    for n in NUMBERS {
        out.push(h.create_contact(c, n).await);
    }
    out
}

/// Every contact still opens with the keys deployed now, to the same masked value.
async fn assert_all_open(h: &Harness, c: &Consumer, created: &[Value]) {
    let scope = privacy_relay::scope::RelayScope::of_delegated(&c.delegated());
    for contact in created {
        let id: Uuid = contact["contactReference"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let row = repository::find(h.conn(), scope, id, false)
            .await
            .unwrap()
            .unwrap();
        let n = crypto::open(
            h.vault_keys.encryption(),
            row.scope(),
            row.id,
            &row.sealed().unwrap(),
        )
        .unwrap();
        assert_eq!(
            n.masked_display(),
            contact["maskedDisplay"].as_str().unwrap()
        );
    }
}

#[tokio::test]
async fn the_admin_cli_rotates_the_encryption_key_add_activate_reencrypt_retire() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let cli = Cli::new(&h.db.url);
    let enc = cli.encryption.clone();
    let mut keys_seen = Vec::new();
    generate(&cli, "ck-cli-1", "fk-cli-1");
    Cli::key_material(&enc, &mut keys_seen);
    h.set_vault_keys(cli.load());
    let c = Consumer::new(ApplicationEnvironmentKind::Production);
    let created = contacts(&h, &c).await;
    let file = Cli::path(&enc);

    // add-key: V2 readable, V1 still active; the previous document is kept as a backup.
    cli.ok(&[
        "contact-vault",
        "add-key",
        "--purpose",
        "encryption",
        "--version",
        "ck-cli-2",
        "--file",
        file,
    ]);
    assert_eq!(
        active(&enc, KeyPurpose::Encryption),
        (
            "ck-cli-1".into(),
            vec!["ck-cli-1".into(), "ck-cli-2".into()]
        )
    );
    assert_eq!((mode(&enc), cli.backups()), (0o600, 1));
    Cli::key_material(&enc, &mut keys_seen);
    // The active version is never retired.
    let e = cli.refused(
        &[
            "contact-vault",
            "retire-key",
            "--purpose",
            "encryption",
            "--version",
            "ck-cli-1",
            "--file",
            file,
        ],
        &enc,
    );
    assert!(e.contains("ck-cli-1"), "{e}");

    // activate-key: V2 active for writes after the redeploy; V1 rows still read.
    cli.ok(&[
        "contact-vault",
        "activate-key",
        "--purpose",
        "encryption",
        "--version",
        "ck-cli-2",
        "--file",
        file,
    ]);
    assert_eq!(active(&enc, KeyPurpose::Encryption).0, "ck-cli-2");
    h.set_vault_keys(cli.load());
    h.contacts.verify_referenced_key_versions().await.unwrap();
    let mut all = created.clone();
    all.push(h.create_contact(&c, "+1 650 253 0000").await);
    assert_all_open(&h, &c, &all).await;
    // V1 is still referenced by three rows: retirement refused, file unchanged.
    let e = cli.refused(
        &[
            "contact-vault",
            "retire-key",
            "--purpose",
            "encryption",
            "--version",
            "ck-cli-1",
            "--file",
            file,
        ],
        &enc,
    );
    assert!(e.contains("still referenced"), "{e}");
    let status = cli.ok(&["contact-vault", "rotation-status"]);
    assert!(status.contains("encryption\tck-cli-1\t3"), "{status}");
    assert!(status.contains("encryption\tck-cli-2\t1"), "{status}");

    // reencrypt: bounded batches until nothing remains.
    let out = cli.ok(&["contact-vault", "reencrypt", "--batch", "2"]);
    assert!(out.contains("rewrapped 2\tfailed 0\tremaining 1"), "{out}");
    assert!(out.contains("rewrapped 1\tfailed 0\tremaining 0"), "{out}");
    assert_eq!(
        h.contacts.key_version_counts().await.unwrap(),
        vec![("ck-cli-2".into(), 4)]
    );
    assert_eq!(
        scalar(
            h.conn(),
            "SELECT count(*) FROM contact_access_audit WHERE operation = 'REENCRYPT' AND outcome = 'SUCCEEDED'"
        )
        .await,
        3
    );

    // retire-key: V1 leaves the file; the redeployed service reads every contact.
    let before_retire = cli.backups();
    cli.ok(&[
        "contact-vault",
        "retire-key",
        "--purpose",
        "encryption",
        "--version",
        "ck-cli-1",
        "--file",
        file,
    ]);
    assert_eq!(
        active(&enc, KeyPurpose::Encryption),
        ("ck-cli-2".into(), vec!["ck-cli-2".into()])
    );
    assert_eq!(cli.backups(), before_retire + 1);
    h.set_vault_keys(cli.load());
    h.contacts.verify_referenced_key_versions().await.unwrap();
    assert_all_open(&h, &c, &all).await;
    // The (only) active version is never retired.
    cli.refused(
        &[
            "contact-vault",
            "retire-key",
            "--purpose",
            "encryption",
            "--version",
            "ck-cli-2",
            "--file",
            file,
        ],
        &enc,
    );

    // A keyring that lacks the referenced version: rotation-status fails and says so.
    let foreign = cli.dir.0.join("foreign-keyring.json");
    cli.ok(&[
        "contact-vault",
        "generate-keyring",
        "--purpose",
        "encryption",
        "--version",
        "ck-cli-9",
        "--out",
        Cli::path(&foreign),
    ]);
    Cli::key_material(&foreign, &mut keys_seen);
    let out = cli.run_with(&foreign, &["contact-vault", "rotation-status"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout)
        .contains("encryption\tck-cli-2\tREFERENCED BUT NOT READABLE"));
    Cli::key_material(&cli.fingerprint, &mut keys_seen);
    cli.assert_nothing_leaked(&keys_seen);
}

#[tokio::test]
async fn the_admin_cli_rotates_the_fingerprint_key_add_backfill_activate_prune_retire() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let cli = Cli::new(&h.db.url);
    let fp = cli.fingerprint.clone();
    let file = Cli::path(&fp);
    let mut keys_seen = Vec::new();
    generate(&cli, "ck-fcli-1", "fk-cli-1");
    h.set_vault_keys(cli.load());
    let c = Consumer::new(ApplicationEnvironmentKind::Production);
    let created = contacts(&h, &c).await;
    let ids: Vec<&str> = created
        .iter()
        .map(|v| v["contactReference"].as_str().unwrap())
        .collect();

    // add-key (V1 stays active) and backfill V2 for every existing contact.
    cli.ok(&[
        "contact-vault",
        "add-key",
        "--purpose",
        "fingerprint",
        "--version",
        "fk-cli-2",
        "--file",
        file,
    ]);
    assert_eq!(active(&fp, KeyPurpose::Fingerprint).0, "fk-cli-1");
    Cli::key_material(&fp, &mut keys_seen);
    h.set_vault_keys(cli.load());
    let out = cli.ok(&["contact-vault", "backfill-fingerprints", "--batch", "2"]);
    assert!(out.contains("remaining 0"), "{out}");
    assert_eq!(
        h.contacts.fingerprint_version_counts().await.unwrap(),
        vec![("fk-cli-1".into(), 3), ("fk-cli-2".into(), 3)]
    );
    // activate-key; V1 is referenced: retirement refused, file unchanged.
    cli.ok(&[
        "contact-vault",
        "activate-key",
        "--purpose",
        "fingerprint",
        "--version",
        "fk-cli-2",
        "--file",
        file,
    ]);
    h.set_vault_keys(cli.load());
    let e = cli.refused(
        &[
            "contact-vault",
            "retire-key",
            "--purpose",
            "fingerprint",
            "--version",
            "fk-cli-1",
            "--file",
            file,
        ],
        &fp,
    );
    assert!(e.contains("still referenced"), "{e}");
    // The active version is never pruned.
    let out = cli.run(&[
        "contact-vault",
        "prune-fingerprints",
        "--version",
        "fk-cli-2",
    ]);
    assert_eq!(out.status.code(), Some(1));
    // prune V1, then retire it.
    let out = cli.ok(&[
        "contact-vault",
        "prune-fingerprints",
        "--version",
        "fk-cli-1",
    ]);
    assert!(out.contains("removed 3 fingerprints of fk-cli-1"), "{out}");
    cli.ok(&[
        "contact-vault",
        "retire-key",
        "--purpose",
        "fingerprint",
        "--version",
        "fk-cli-1",
        "--file",
        file,
    ]);
    assert_eq!(
        active(&fp, KeyPurpose::Fingerprint),
        ("fk-cli-2".into(), vec!["fk-cli-2".into()])
    );
    // Redeployed on the V2-only keyring: equality still finds every contact (no duplicate).
    h.set_vault_keys(cli.load());
    h.contacts.verify_referenced_key_versions().await.unwrap();
    for (n, id) in NUMBERS.iter().zip(&ids) {
        let again = h.create_contact(&c, n).await;
        assert_eq!(again["contactReference"].as_str(), Some(*id), "{n}");
    }
    assert_eq!(
        scalar(h.conn(), "SELECT count(*) FROM protected_contacts").await,
        3
    );
    Cli::key_material(&cli.encryption, &mut keys_seen);
    Cli::key_material(&fp, &mut keys_seen);
    cli.assert_nothing_leaked(&keys_seen);
}
