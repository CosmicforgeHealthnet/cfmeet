//! Part M — runtime route coverage. Every request any integration test sends through
//! `TestApp::router` is recorded as `method \t matched route \t status \t test` in
//! `$CARGO_TARGET_TMPDIR/route-coverage/<binary>-<pid>.tsv`.
//! `scripts/certification/route_coverage.py` joins these records with the OpenAPI
//! contract to prove that every `/api/v2` operation has a success-path and an
//! error-path integration test (measured, not a hand-maintained list).
#![allow(dead_code)]

use std::{
    fs::{File, OpenOptions},
    io::Write,
    sync::{Mutex, OnceLock},
};

use axum::{extract::MatchedPath, extract::Request, middleware::Next, response::Response};

fn sink() -> &'static Mutex<Option<File>> {
    static SINK: OnceLock<Mutex<Option<File>>> = OnceLock::new();
    SINK.get_or_init(|| {
        let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("route-coverage");
        let file = std::fs::create_dir_all(&dir).ok().and_then(|_| {
            let bin = std::env::current_exe()
                .ok()
                .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().to_string()))
                .unwrap_or_else(|| "tests".into());
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join(format!("{bin}-{}.tsv", std::process::id())))
                .ok()
        });
        Mutex::new(file)
    })
}

pub async fn record(request: Request, next: Next) -> Response {
    let method = request.method().to_string();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_string())
        .unwrap_or_default();
    let response = next.run(request).await;
    let test = std::thread::current().name().unwrap_or("").to_string();
    if let Ok(mut guard) = sink().lock() {
        if let Some(f) = guard.as_mut() {
            let _ = writeln!(
                f,
                "{method}\t{route}\t{}\t{test}",
                response.status().as_u16()
            );
        }
    }
    response
}
