//! `security-scan-gateway-keygen <kid> <private-key-file>`
//!
//! Creates the gateway's verdict signing key (PKCS#8 PEM, mode 0600) if the file does not
//! exist; an existing key is NEVER replaced. Prints the public verdict key set
//! `{"keys":[{"kid","x"}]}` that callers trust. No secret is printed.

use std::{fs::OpenOptions, io::Write, os::unix::fs::OpenOptionsExt, path::Path};

use ed25519_dalek::{
    pkcs8::{DecodePrivateKey, EncodePrivateKey},
    SigningKey,
};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: security-scan-gateway-keygen <kid> <private-key-file>");
        std::process::exit(64);
    }
    let (kid, path) = (&args[1], Path::new(&args[2]));
    let key = if path.exists() {
        let pem = std::fs::read_to_string(path).expect("readable key file");
        SigningKey::from_pkcs8_pem(&pem).expect("existing file is a PKCS#8 Ed25519 key")
    } else {
        let key = SigningKey::generate(&mut rand_core::OsRng);
        let pem = key
            .to_pkcs8_pem(ed25519_dalek::pkcs8::spki::der::pem::LineEnding::LF)
            .expect("encodable key");
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .expect("new key file");
        f.write_all(pem.as_bytes()).expect("written key");
        key
    };
    let public = cf_scan_gateway_contract::public_key(kid, &key);
    println!("{}", serde_json::json!({ "keys": [public] }));
}
