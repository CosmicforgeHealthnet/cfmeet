//! CF Meet Messaging cryptographic core (Part V Workstream A, ADR 0045).
//!
//! Conceptual separation (Part V Appendix G / L):
//!
//! * **Messaging backend** — routing, authorization, metadata and durable state. It uses
//!   only the PUBLIC half of this crate: [`key_package`] (structural and signature
//!   validation of published MLS KeyPackages), [`binding`] (the canonical device
//!   credential and registration-challenge formats) and [`transparency`] (the
//!   append-only Merkle log and its proofs). None of these touches a private key.
//! * **Client crypto core** — [`device`] (feature `client`): device signature keys,
//!   KeyPackage generation and the registration proof of possession. Private keys live
//!   only in the client process (the in-memory OpenMLS provider here; durable device
//!   storage belongs to the client SDKs).
//!
//! No cryptographic primitive is implemented here. MLS is OpenMLS 0.9.1 (RFC 9420); the
//! transparency log is the RFC 9162 §2.1 Merkle tree over SHA-256.
//!
//! The only cipher suite accepted is the RFC 9420 mandatory-to-implement
//! `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519` ([`profile::MANDATORY_CIPHERSUITE`]).
//! MLS 1.0 does not provide post-quantum confidentiality, and this crate makes no Signal
//! Protocol interoperability claim.

pub mod binding;
pub mod framing;
pub mod key_package;
pub mod profile;
pub mod transparency;

#[cfg(feature = "client")]
pub mod attachment;
#[cfg(feature = "client")]
pub mod device;
#[cfg(feature = "client")]
pub mod group;
#[cfg(feature = "client")]
pub mod local_search;
