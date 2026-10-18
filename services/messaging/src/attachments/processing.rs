//! The future media-processing interface (§80): asynchronous derivatives (thumbnails,
//! transcodes, audio waveforms, previews, OCR …) WITHOUT a processing farm today.
//!
//! * A processor consumes Messaging-owned references ([`ProcessingRequest`]: attachment id
//!   and object key in the protected store) and produces NEW attachments of the same
//!   conversation; an original attachment's identity never changes.
//! * [`eligible`] is the only gate: MANAGED and READY only. PRIVATE_E2EE attachments are
//!   ciphertext and are NEVER handed to a server processor; their derivatives (a
//!   thumbnail, a waveform) are created by the sending client, encrypted like any
//!   attachment and referenced from the MLS-protected descriptor ([`E2eeDerivative`]
//!   documents that contract).
//! * No processor is registered in this deployment ([`registered`] is empty): nothing
//!   claims a derivative exists.

use uuid::Uuid;

use super::AttachmentRow;
use crate::domain::SecurityProfile;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Derivative {
    Thumbnail,
    Transcode,
    Waveform,
    Preview,
    TextExtraction,
}

/// What a server-side processor receives (references, never inline bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessingRequest {
    pub attachment_id: Uuid,
    pub conversation_id: Uuid,
    pub object_key: String,
    pub media_type: String,
    pub derivative: Derivative,
}

/// How an E2EE client represents a derivative it created (inside the MLS descriptor).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct E2eeDerivative {
    pub derivative: Derivative,
    /// A separate encrypted attachment holding the derivative.
    pub attachment_id: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ineligible {
    EndToEndEncrypted,
    NotReady,
}

/// Whether an attachment may be given to a SERVER-side processor.
pub fn eligible(
    row: &AttachmentRow,
    derivative: Derivative,
) -> Result<ProcessingRequest, Ineligible> {
    if row.profile() == SecurityProfile::PrivateE2ee {
        return Err(Ineligible::EndToEndEncrypted);
    }
    if row.state != "READY" {
        return Err(Ineligible::NotReady);
    }
    Ok(ProcessingRequest {
        attachment_id: row.id,
        conversation_id: row.conversation_id,
        object_key: row.object_key.clone(),
        media_type: row.media_type.clone(),
        derivative,
    })
}

/// Processors registered in this deployment (none yet).
pub fn registered() -> &'static [Derivative] {
    &[]
}
