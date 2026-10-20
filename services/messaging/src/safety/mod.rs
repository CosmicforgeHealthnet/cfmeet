//! Safety (Part V §85–§88): direct-message [`blocks`], metadata-based [`abuse`] controls,
//! user [`reports`] and authorization-aware [`moderation`]. Nothing here inspects
//! PRIVATE_E2EE content: E2EE reports carry only what the reporting client chose to
//! disclose, and abuse signals are metadata (rates, counts), never content.

pub mod abuse;
pub mod blocks;
pub mod http;
