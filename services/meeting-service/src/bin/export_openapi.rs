//! Export the Meeting Service OpenAPI document (authoritative consumer contract).
//!
//! `cargo run --bin export_openapi > ../../packages/contracts/openapi/meeting-service.json`

fn main() {
    println!("{}", backend::swagger::openapi_json());
}
