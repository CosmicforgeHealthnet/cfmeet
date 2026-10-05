//! Phase 2 Part I — meeting quality, operations & observability (internal modules of
//! the meeting-service).
//!
//! - [`sink`]: `QualityTelemetrySink` (vendor-neutral; Prometheus exporter default).
//! - [`health`]: centralized, configurable health classification.
//! - [`service`]: `QualityService` (authenticated, validated, rate-limited sample
//!   ingestion → low-frequency PostgreSQL aggregates) and join-SLI classification.
//! - [`operations`]: `OperationsService` (internal conference health + incidents).
//! - [`incidents`]: operational incidents over the generalised drift store.

pub mod health;
pub mod incidents;
pub mod operations;
pub mod service;
pub mod sink;
