//! `cf-service-keygen` — operational key tooling for internal service authentication.
//!
//! ```text
//! cf-service-keygen generate --kid <kid> --out <private-key-file>
//!     Writes a new Ed25519 PKCS#8 PEM private key (mode 0600, never overwrites) and prints
//!     its public JWK for receivers' trust bundles. The private key is never printed.
//!
//! cf-service-keygen public-jwk --kid <kid> --key <private-key-file>
//!     Prints the public JWK of an existing private key.
//! ```

use std::{io::Write, path::PathBuf, process::ExitCode};

use cf_service_auth::{KeyId, ServiceSigningKey};

fn usage() -> ExitCode {
    eprintln!(
        "usage:\n  cf-service-keygen generate --kid <kid> --out <private-key-file>\n  cf-service-keygen public-jwk --kid <kid> --key <private-key-file>"
    );
    ExitCode::from(2)
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == name).map(|w| w[1].clone())
}

fn print_jwk(key: &ServiceSigningKey) -> ExitCode {
    match serde_json::to_string_pretty(&key.public_jwk()) {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(_) => ExitCode::FAILURE,
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first() else {
        return usage();
    };
    let Some(kid) = flag(&args, "--kid").and_then(|k| KeyId::parse(&k).ok()) else {
        eprintln!("cf-service-keygen: --kid is required (1-64 of [A-Za-z0-9._-])");
        return usage();
    };
    match command.as_str() {
        "generate" => {
            let Some(out) = flag(&args, "--out").map(PathBuf::from) else {
                return usage();
            };
            let key = ServiceSigningKey::generate(kid);
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let written = options
                .open(&out)
                .and_then(|mut f| f.write_all(key.to_pkcs8_pem().as_bytes()));
            if written.is_err() {
                eprintln!(
                    "cf-service-keygen: cannot create {} (it must not already exist)",
                    out.display()
                );
                return ExitCode::FAILURE;
            }
            eprintln!(
                "cf-service-keygen: private key written to {}",
                out.display()
            );
            print_jwk(&key)
        }
        "public-jwk" => {
            let Some(path) = flag(&args, "--key").map(PathBuf::from) else {
                return usage();
            };
            match ServiceSigningKey::from_pkcs8_pem_file(kid, &path) {
                Ok(key) => print_jwk(&key),
                Err(e) => {
                    eprintln!("cf-service-keygen: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => usage(),
    }
}
