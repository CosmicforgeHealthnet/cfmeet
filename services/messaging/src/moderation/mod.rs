//! User reports and moderation (Part V §85–§87).
//!
//! * [`reports`]: members report a conversation, messages or a principal. In a
//!   PRIVATE_E2EE conversation the server has no plaintext: a report carries metadata
//!   (message ids, sequences, ciphertext digests, acceptance records) and — ONLY if the
//!   reporter chooses to — what the reporter's own device decrypted. That disclosure is
//!   sealed at rest and always presented as reporter-attested (the server cannot verify
//!   it against the ciphertext). MANAGED reports reference server-visible messages, shown
//!   to moderators only where policy allows. The reported principal is never told who
//!   reported.
//! * [`actions`]: application moderators (scope `moderation.manage`, no acting principal)
//!   remove messages (tombstone `MODERATED`, then purge), mute / unmute and remove
//!   members, restrict and un-restrict principals and dismiss reports. Every action is an
//!   append-only record plus an audit entry; none of them can read E2EE content.

pub mod actions;
pub mod http;
pub mod reports;
