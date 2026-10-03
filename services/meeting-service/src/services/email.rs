//! Email Service
//!
//! High-level email service that enqueues emails for async delivery.

use std::sync::Arc;
use uuid::Uuid;

use crate::config::EmailConfig;
use crate::error::ApiResult;
use crate::queues::RedisEmailQueue;
use crate::services::RedisService;
use crate::templates::email::{
    notification_email, password_reset_email, verification_email, welcome_email,
};

/// High-level email service for the application.
#[derive(Clone)]
pub struct EmailService {
    queue: RedisEmailQueue,
    app_url: String,
}

impl EmailService {
    /// Create a new EmailService
    pub fn new(redis: Arc<RedisService>, config: &EmailConfig) -> Self {
        Self {
            queue: RedisEmailQueue::new(redis),
            app_url: config.app_url.clone(),
        }
    }

    /// Get the underlying queue for worker access
    pub fn queue(&self) -> &RedisEmailQueue {
        &self.queue
    }

    /// Send verification email
    pub async fn send_verification_email(
        &self,
        to_email: &str,
        username: &str,
        verification_code: &str,
    ) -> ApiResult<Uuid> {
        let template = verification_email(username, verification_code);

        self.queue
            .enqueue(
                to_email,
                Some(username),
                "Verify Your Email - CosmicForge HealthNet",
                &template.html,
                &template.text,
            )
            .await
    }

    /// Send password reset email
    pub async fn send_password_reset_email(
        &self,
        to_email: &str,
        username: &str,
        reset_code: &str,
    ) -> ApiResult<Uuid> {
        let template = password_reset_email(username, reset_code);

        self.queue
            .enqueue(
                to_email,
                Some(username),
                "Reset Your Password - CosmicForge HealthNet",
                &template.html,
                &template.text,
            )
            .await
    }

    /// Send welcome email
    pub async fn send_welcome_email(&self, to_email: &str, username: &str) -> ApiResult<Uuid> {
        let template = welcome_email(username, &self.app_url);

        self.queue
            .enqueue(
                to_email,
                Some(username),
                "Welcome to CosmicForge HealthNet!",
                &template.html,
                &template.text,
            )
            .await
    }

    /// Send notification email
    pub async fn send_notification_email(
        &self,
        to_email: &str,
        username: &str,
        subject: &str,
        message: &str,
    ) -> ApiResult<Uuid> {
        let template = notification_email(username, subject, message);

        self.queue
            .enqueue(to_email, Some(username), subject, &template.html, &template.text)
            .await
    }
}
