//! Email Worker
//!
//! Background worker that processes queued email jobs.

use lettre::{
    message::{header::ContentType, Mailbox},
    transport::smtp::authentication::Credentials,
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};
use std::sync::Arc;
use tokio::sync::watch;
use tokio::time::{sleep, Duration};

use crate::config::EmailConfig;
use crate::queues::{redis_email::EmailJob, RedisEmailQueue};

/// Background worker for processing email jobs
pub struct EmailWorker {
    queue: RedisEmailQueue,
    mailer: Arc<AsyncSmtpTransport<Tokio1Executor>>,
    from_email: String,
    from_name: String,
}

impl EmailWorker {
    /// Create a new EmailWorker
    pub fn new(queue: RedisEmailQueue, config: &EmailConfig) -> Result<Self, String> {
        let credentials =
            Credentials::new(config.smtp_username.clone(), config.smtp_password.clone());

        let mailer = if config.smtp_port == 465 {
            AsyncSmtpTransport::<Tokio1Executor>::relay(&config.smtp_host)
                .map_err(|e| format!("Failed to create SMTP transport: {}", e))?
                .port(config.smtp_port)
                .credentials(credentials)
                .build()
        } else {
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.smtp_host)
                .map_err(|e| format!("Failed to create SMTP transport: {}", e))?
                .port(config.smtp_port)
                .credentials(credentials)
                .build()
        };

        Ok(Self {
            queue,
            mailer: Arc::new(mailer),
            from_email: config.from_email.clone(),
            from_name: config.from_name.clone(),
        })
    }

    /// Start the worker loop
    pub async fn run(&self, mut shutdown_rx: watch::Receiver<bool>) {
        tracing::info!(
            "Email worker started (from: {} <{}>)",
            self.from_name,
            self.from_email
        );

        loop {
            if *shutdown_rx.borrow() {
                tracing::info!("Email worker shutting down");
                break;
            }

            match self.queue.pop(5).await {
                Ok(Some(job)) => {
                    self.process_job(job).await;
                }
                Ok(None) => {
                    // No jobs, continue waiting
                }
                Err(e) => {
                    tracing::error!("Error popping job: {}", e);
                    sleep(Duration::from_secs(1)).await;
                }
            }

            // Check shutdown
            tokio::select! {
                biased;
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        tracing::info!("Email worker shutting down");
                        break;
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
        }
    }

    /// Process a single email job
    async fn process_job(&self, job: EmailJob) {
        let job_id = job.id.clone();

        match self.send_email(&job).await {
            Ok(()) => {
                tracing::info!("Email sent: {} -> {}", job_id, job.to_email);
            }
            Err(e) => {
                tracing::warn!("Email job {} failed: {}", job_id, e);
                if let Err(retry_err) = self.queue.retry(job).await {
                    tracing::error!("Failed to retry job {}: {}", job_id, retry_err);
                }
            }
        }
    }

    /// Send email via SMTP
    async fn send_email(&self, job: &EmailJob) -> Result<(), String> {
        let from_mailbox: Mailbox = format!("{} <{}>", self.from_name, self.from_email)
            .parse()
            .map_err(|e| format!("Invalid from email: {}", e))?;

        let to_display = job.to_name.as_deref().unwrap_or(&job.to_email);
        let to_mailbox: Mailbox = format!("{} <{}>", to_display, job.to_email)
            .parse()
            .map_err(|e| format!("Invalid recipient email: {}", e))?;

        let email = Message::builder()
            .from(from_mailbox)
            .to(to_mailbox)
            .subject(&job.subject)
            .multipart(
                lettre::message::MultiPart::alternative()
                    .singlepart(
                        lettre::message::SinglePart::builder()
                            .header(ContentType::TEXT_PLAIN)
                            .body(job.text_body.clone()),
                    )
                    .singlepart(
                        lettre::message::SinglePart::builder()
                            .header(ContentType::TEXT_HTML)
                            .body(job.html_body.clone()),
                    ),
            )
            .map_err(|e| format!("Failed to build email: {}", e))?;

        self.mailer
            .send(email)
            .await
            .map_err(|e| format!("SMTP error: {}", e))?;

        Ok(())
    }
}

/// Handle for controlling the email worker
pub struct EmailWorkerHandle {
    shutdown_tx: watch::Sender<bool>,
}

impl EmailWorkerHandle {
    /// Signal the worker to shut down
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}

/// Spawn the email worker as a background task
pub fn spawn_email_worker(
    queue: RedisEmailQueue,
    config: &EmailConfig,
) -> Option<EmailWorkerHandle> {
    let worker = match EmailWorker::new(queue, config) {
        Ok(w) => w,
        Err(e) => {
            tracing::error!("Failed to create email worker: {}", e);
            return None;
        }
    };

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    tokio::spawn(async move {
        worker.run(shutdown_rx).await;
    });

    Some(EmailWorkerHandle { shutdown_tx })
}
