//! Quick SMTP test - run with: cargo run --bin test_email

use dotenvy::dotenv;
use lettre::{
    message::Mailbox, transport::smtp::authentication::Credentials, AsyncSmtpTransport,
    AsyncTransport, Message, Tokio1Executor,
};

fn get_env(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| {
        eprintln!("❌ Missing env var: {}", key);
        std::process::exit(1);
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load .env from current directory
    match dotenv() {
        Ok(path) => println!("Loaded .env from: {:?}", path),
        Err(e) => println!("Note: No .env file found ({}), using system env vars", e),
    }

    let smtp_host = get_env("SMTP_HOST");
    let smtp_port: u16 = get_env("SMTP_PORT").parse().expect("Invalid SMTP_PORT");
    let smtp_username = get_env("SMTP_USERNAME");
    let smtp_password = get_env("SMTP_PASSWORD");
    let from_email = get_env("SMTP_FROM_EMAIL");

    println!("=== SMTP Test ===");
    println!("Host: {}:{}", smtp_host, smtp_port);
    println!("Username: {}", smtp_username);
    println!("From: {}", from_email);
    println!();

    // Build credentials
    let credentials = Credentials::new(smtp_username.clone(), smtp_password);

    // Try STARTTLS (port 587)
    println!("Connecting to SMTP server...");
    let mailer = AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&smtp_host)?
        .port(smtp_port)
        .credentials(credentials)
        .build();

    // Build test email
    let from_mailbox: Mailbox = format!("Test <{}>", from_email).parse()?;
    let to_mailbox: Mailbox = format!("Test <{}>", smtp_username).parse()?;

    let email = Message::builder()
        .from(from_mailbox)
        .to(to_mailbox)
        .subject("SMTP Test - CosmicForge")
        .body("This is a test email from the CosmicForge backend.".to_string())?;

    println!("Sending test email to {}...", smtp_username);

    match mailer.send(email).await {
        Ok(response) => {
            println!("✅ Email sent successfully!");
            println!("Response: {:?}", response);
        }
        Err(e) => {
            println!("❌ Failed to send email!");
            println!("Error: {}", e);
            println!();
            println!("Common issues:");
            println!("  1. App Password incorrect or expired");
            println!("  2. 2FA not enabled on Gmail (required for App Passwords)");
            println!("  3. Gmail account security settings blocking access");
        }
    }

    Ok(())
}
