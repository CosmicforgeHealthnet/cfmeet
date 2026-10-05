//! Parts G–I tunables: artefact storage, recording finalization, retention worker,
//! and the centralized quality / health thresholds (no magic numbers in handlers).

use super::features::{env_bool, env_num};

fn env_str(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// S3-compatible object storage (AWS S3, MinIO, Cloudflare R2, …).
#[derive(Debug, Clone, Default)]
pub struct ObjectStorageConfig {
    /// Server-side endpoint, e.g. `https://s3.eu-west-1.amazonaws.com` or
    /// `http://minio:9000`. `None` → artefact storage is not configured.
    pub endpoint: Option<String>,
    /// Endpoint used to sign client read URLs (defaults to `endpoint`).
    pub public_endpoint: Option<String>,
    pub region: String,
    pub bucket: String,
    pub access_key: String,
    pub secret_key: String,
    /// Path-style addressing (MinIO / most self-hosted stores).
    pub path_style: bool,
    /// Key prefix for every artefact object.
    pub prefix: String,
}

impl ObjectStorageConfig {
    pub fn from_env() -> Self {
        Self {
            endpoint: env_str("ARTIFACT_STORAGE_ENDPOINT"),
            public_endpoint: env_str("ARTIFACT_STORAGE_PUBLIC_ENDPOINT"),
            region: env_str("ARTIFACT_STORAGE_REGION").unwrap_or_else(|| "us-east-1".into()),
            bucket: env_str("ARTIFACT_STORAGE_BUCKET").unwrap_or_else(|| "cfmeet-artifacts".into()),
            access_key: env_str("ARTIFACT_STORAGE_ACCESS_KEY").unwrap_or_default(),
            secret_key: env_str("ARTIFACT_STORAGE_SECRET_KEY").unwrap_or_default(),
            path_style: env_bool("ARTIFACT_STORAGE_PATH_STYLE", true),
            prefix: env_str("ARTIFACT_STORAGE_PREFIX").unwrap_or_else(|| "artifacts".into()),
        }
    }

    pub fn is_configured(&self) -> bool {
        self.endpoint.is_some() && !self.access_key.is_empty() && !self.secret_key.is_empty()
    }
}

/// Recording (composite egress) encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingEncoding {
    /// Provider default preset (H.264 + AAC MP4 on LiveKit).
    ProviderDefault,
    /// H.264 + Opus MP4 (no AAC encoder required on the egress host).
    H264Opus,
}

#[derive(Debug, Clone)]
pub struct ArtifactConfig {
    pub storage: ObjectStorageConfig,
    /// Signed read URL lifetime (bounded).
    pub signed_url_ttl_secs: u64,
    /// Seconds a REQUESTED recording may wait for provider start evidence.
    pub recording_start_timeout_secs: i64,
    /// Seconds a STOPPING recording may wait for provider completion before the
    /// reconciler queries the provider (lost callback).
    pub recording_stop_timeout_secs: i64,
    /// Finalization attempts before a PROCESSING artefact whose object never
    /// appears becomes FAILED (ARTIFACT_MISSING).
    pub finalize_max_attempts: i32,
    /// Largest object hashed for integrity validation.
    pub max_checksum_bytes: u64,
    pub recording_encoding: RecordingEncoding,
    /// Retention worker batch / cadence.
    pub retention_batch: u64,
    pub retention_poll_secs: u64,
    /// Shared secret for provider-neutral transcription callbacks.
    pub transcription_webhook_secret: Option<String>,
}

impl Default for ArtifactConfig {
    fn default() -> Self {
        Self {
            storage: ObjectStorageConfig::default(),
            signed_url_ttl_secs: 300,
            recording_start_timeout_secs: 90,
            recording_stop_timeout_secs: 120,
            finalize_max_attempts: 10,
            max_checksum_bytes: 8 * 1024 * 1024 * 1024,
            recording_encoding: RecordingEncoding::ProviderDefault,
            retention_batch: 50,
            retention_poll_secs: 60,
            transcription_webhook_secret: None,
        }
    }
}

impl ArtifactConfig {
    pub fn from_env() -> Self {
        let d = Self::default();
        Self {
            storage: ObjectStorageConfig::from_env(),
            signed_url_ttl_secs: env_num("ARTIFACT_SIGNED_URL_TTL_SECS", d.signed_url_ttl_secs)
                .clamp(30, 3600),
            recording_start_timeout_secs: env_num(
                "RECORDING_START_TIMEOUT_SECS",
                d.recording_start_timeout_secs,
            )
            .max(10),
            recording_stop_timeout_secs: env_num(
                "RECORDING_STOP_TIMEOUT_SECS",
                d.recording_stop_timeout_secs,
            )
            .max(10),
            finalize_max_attempts: env_num(
                "ARTIFACT_FINALIZE_MAX_ATTEMPTS",
                d.finalize_max_attempts,
            )
            .clamp(1, 100),
            max_checksum_bytes: env_num("ARTIFACT_MAX_CHECKSUM_BYTES", d.max_checksum_bytes),
            recording_encoding: match env_str("RECORDING_ENCODING").as_deref() {
                Some("h264-opus") => RecordingEncoding::H264Opus,
                _ => RecordingEncoding::ProviderDefault,
            },
            retention_batch: env_num("ARTIFACT_RETENTION_BATCH", d.retention_batch).clamp(1, 1000),
            retention_poll_secs: env_num("ARTIFACT_RETENTION_POLL_SECS", d.retention_poll_secs)
                .max(1),
            transcription_webhook_secret: env_str("TRANSCRIPTION_WEBHOOK_SECRET"),
        }
    }
}

/// Health thresholds (Part I §65 / §87). `degraded` < `unhealthy` for every signal.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Threshold {
    pub degraded: f64,
    pub unhealthy: f64,
}

impl Threshold {
    const fn new(degraded: f64, unhealthy: f64) -> Self {
        Self {
            degraded,
            unhealthy,
        }
    }

    fn from_env(prefix: &str, d: Threshold) -> Self {
        let degraded = env_num(&format!("{prefix}_DEGRADED"), d.degraded);
        let unhealthy = env_num(&format!("{prefix}_UNHEALTHY"), d.unhealthy);
        if degraded.is_finite() && unhealthy.is_finite() && degraded <= unhealthy {
            Self {
                degraded,
                unhealthy,
            }
        } else {
            tracing::warn!("{prefix}_* thresholds invalid; using defaults");
            d
        }
    }
}

#[derive(Debug, Clone)]
pub struct QualityConfig {
    pub rtt_ms: Threshold,
    pub jitter_ms: Threshold,
    pub packet_loss_ratio: Threshold,
    /// Bitrate below `degraded` / `unhealthy` (kbps; lower is worse).
    pub low_bitrate_kbps: Threshold,
    pub join_latency_ms: Threshold,
    pub join_failure_rate: Threshold,
    pub reconnects: Threshold,
    pub unhealthy_participant_ratio: Threshold,
    /// Client sample ingestion bounds.
    pub max_samples_per_batch: usize,
    pub max_batches_per_minute: u64,
    pub max_body_bytes: usize,
    /// Clock skew tolerated on client sample timestamps.
    pub max_sample_skew_secs: i64,
    /// Seconds a credentialed session may stay PENDING before it counts as a join
    /// timeout.
    pub join_timeout_secs: i64,
}

impl Default for QualityConfig {
    fn default() -> Self {
        Self {
            rtt_ms: Threshold::new(300.0, 800.0),
            jitter_ms: Threshold::new(30.0, 100.0),
            packet_loss_ratio: Threshold::new(0.03, 0.10),
            low_bitrate_kbps: Threshold::new(150.0, 50.0),
            join_latency_ms: Threshold::new(3_000.0, 10_000.0),
            join_failure_rate: Threshold::new(0.05, 0.20),
            reconnects: Threshold::new(3.0, 8.0),
            unhealthy_participant_ratio: Threshold::new(0.20, 0.50),
            max_samples_per_batch: 20,
            max_batches_per_minute: 12,
            max_body_bytes: 16 * 1024,
            max_sample_skew_secs: 300,
            join_timeout_secs: 45,
        }
    }
}

impl QualityConfig {
    pub fn from_env() -> Self {
        let d = Self::default();
        // Low bitrate is inverted (lower is worse): degraded > unhealthy.
        let low_bitrate_kbps = {
            let degraded = env_num(
                "QUALITY_LOW_BITRATE_KBPS_DEGRADED",
                d.low_bitrate_kbps.degraded,
            );
            let unhealthy = env_num(
                "QUALITY_LOW_BITRATE_KBPS_UNHEALTHY",
                d.low_bitrate_kbps.unhealthy,
            );
            if degraded.is_finite() && unhealthy.is_finite() && unhealthy <= degraded {
                Threshold::new(degraded, unhealthy)
            } else {
                d.low_bitrate_kbps
            }
        };
        Self {
            rtt_ms: Threshold::from_env("QUALITY_RTT_MS", d.rtt_ms),
            jitter_ms: Threshold::from_env("QUALITY_JITTER_MS", d.jitter_ms),
            packet_loss_ratio: Threshold::from_env("QUALITY_PACKET_LOSS", d.packet_loss_ratio),
            low_bitrate_kbps,
            join_latency_ms: Threshold::from_env("QUALITY_JOIN_LATENCY_MS", d.join_latency_ms),
            join_failure_rate: Threshold::from_env(
                "QUALITY_JOIN_FAILURE_RATE",
                d.join_failure_rate,
            ),
            reconnects: Threshold::from_env("QUALITY_RECONNECTS", d.reconnects),
            unhealthy_participant_ratio: Threshold::from_env(
                "QUALITY_UNHEALTHY_PARTICIPANT_RATIO",
                d.unhealthy_participant_ratio,
            ),
            max_samples_per_batch: env_num(
                "QUALITY_MAX_SAMPLES_PER_BATCH",
                d.max_samples_per_batch,
            )
            .clamp(1, 100),
            max_batches_per_minute: env_num(
                "QUALITY_MAX_BATCHES_PER_MINUTE",
                d.max_batches_per_minute,
            )
            .clamp(1, 600),
            max_body_bytes: env_num("QUALITY_MAX_BODY_BYTES", d.max_body_bytes)
                .clamp(1024, 256 * 1024),
            max_sample_skew_secs: env_num("QUALITY_MAX_SAMPLE_SKEW_SECS", d.max_sample_skew_secs)
                .clamp(5, 3600),
            join_timeout_secs: env_num("QUALITY_JOIN_TIMEOUT_SECS", d.join_timeout_secs)
                .clamp(5, 600),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_thresholds_are_ordered() {
        let q = QualityConfig::default();
        for t in [
            q.rtt_ms,
            q.jitter_ms,
            q.packet_loss_ratio,
            q.join_latency_ms,
            q.join_failure_rate,
            q.reconnects,
            q.unhealthy_participant_ratio,
        ] {
            assert!(t.degraded < t.unhealthy);
        }
        assert!(q.low_bitrate_kbps.unhealthy < q.low_bitrate_kbps.degraded);
    }

    #[test]
    fn storage_is_unconfigured_by_default() {
        assert!(!ObjectStorageConfig::default().is_configured());
        assert!(ArtifactConfig::default().signed_url_ttl_secs <= 3600);
    }
}
