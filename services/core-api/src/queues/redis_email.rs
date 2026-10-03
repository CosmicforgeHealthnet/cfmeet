//! Redis Email Queue
//!
//! Simple email queue using Redis List (LPUSH/BRPOP).

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};
use crate::services::RedisService;

/// Email queue key
const EMAIL_QUEUE: &str = "email:queue";

/// Maximum retries before dropping job
const MAX_RETRIES: u32 = 3;

/// Email job data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailJob {
    pub id: String,
    pub to_email: String,
    pub to_name: Option<String>,
    pub subject: String,
    pub html_body: String,
    pub text_body: String,
    pub retry_count: u32,
}

impl EmailJob {
    /// Create a new email job
    pub fn new(
        to_email: &str,
        to_name: Option<&str>,
        subject: &str,
        html_body: &str,
        text_body: &str,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            to_email: to_email.to_string(),
            to_name: to_name.map(|s| s.to_string()),
            subject: subject.to_string(),
            html_body: html_body.to_string(),
            text_body: text_body.to_string(),
            retry_count: 0,
        }
    }
}

/// Redis Email Queue service
#[derive(Clone)]
pub struct RedisEmailQueue {
    redis: Arc<RedisService>,
}

impl RedisEmailQueue {
    /// Create a new RedisEmailQueue
    pub fn new(redis: Arc<RedisService>) -> Self {
        Self { redis }
    }

    /// Enqueue an email job
    pub async fn enqueue(
        &self,
        to_email: &str,
        to_name: Option<&str>,
        subject: &str,
        html_body: &str,
        text_body: &str,
    ) -> ApiResult<Uuid> {
        let job = EmailJob::new(to_email, to_name, subject, html_body, text_body);
        let job_id = Uuid::parse_str(&job.id)
            .map_err(|e| ApiError::InternalError(format!("Invalid UUID: {}", e)))?;

        let job_json = serde_json::to_string(&job)
            .map_err(|e| ApiError::InternalError(format!("JSON serialization error: {}", e)))?;

        let mut conn = self.redis.get_conn().await?;
        let _: i64 = redis::cmd("LPUSH")
            .arg(EMAIL_QUEUE)
            .arg(&job_json)
            .query_async(&mut conn)
            .await
            .map_err(|e| ApiError::InternalError(format!("Redis LPUSH error: {}", e)))?;

        tracing::info!("Email job queued: {} -> {}", job.id, to_email);
        Ok(job_id)
    }

    /// Pop a job from the queue (blocking with timeout)
    pub async fn pop(&self, timeout_secs: u64) -> ApiResult<Option<EmailJob>> {
        let mut conn = self.redis.get_conn().await?;

        let result: Option<(String, String)> = redis::cmd("BRPOP")
            .arg(EMAIL_QUEUE)
            .arg(timeout_secs)
            .query_async(&mut conn)
            .await
            .map_err(|e| ApiError::InternalError(format!("Redis BRPOP error: {}", e)))?;

        match result {
            Some((_, job_json)) => {
                let job: EmailJob = serde_json::from_str(&job_json)
                    .map_err(|e| ApiError::InternalError(format!("JSON parse error: {}", e)))?;
                Ok(Some(job))
            }
            None => Ok(None),
        }
    }

    /// Re-queue a job for retry
    pub async fn retry(&self, mut job: EmailJob) -> ApiResult<bool> {
        job.retry_count += 1;

        if job.retry_count >= MAX_RETRIES {
            tracing::warn!("Email job {} exceeded max retries, dropping", job.id);
            return Ok(false);
        }

        let job_json = serde_json::to_string(&job)
            .map_err(|e| ApiError::InternalError(format!("JSON serialization error: {}", e)))?;

        let mut conn = self.redis.get_conn().await?;
        let _: i64 = redis::cmd("RPUSH")
            .arg(EMAIL_QUEUE)
            .arg(&job_json)
            .query_async(&mut conn)
            .await
            .map_err(|e| ApiError::InternalError(format!("Redis RPUSH error: {}", e)))?;

        tracing::info!("Email job {} re-queued (attempt {})", job.id, job.retry_count);
        Ok(true)
    }

    /// Get queue length
    pub async fn len(&self) -> ApiResult<i64> {
        let mut conn = self.redis.get_conn().await?;

        let len: i64 = redis::cmd("LLEN")
            .arg(EMAIL_QUEUE)
            .query_async(&mut conn)
            .await
            .map_err(|e| ApiError::InternalError(format!("Redis LLEN error: {}", e)))?;

        Ok(len)
    }
}
