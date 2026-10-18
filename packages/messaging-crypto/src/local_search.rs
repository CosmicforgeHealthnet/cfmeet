//! Client-local search over decrypted PRIVATE_E2EE messages (feature `client`; Part V
//! §81–§83).
//!
//! Full-content search of an end-to-end encrypted conversation happens ONLY on an
//! authorized device: the backend never receives search terms, tokens, snippets or
//! counts. [`LocalIndex`] is the reusable core client SDKs wrap:
//!
//! * an in-memory inverted index (Unicode-lower-cased word tokens, prefix queries) that
//!   the device fills as it decrypts messages and prunes when messages are deleted,
//!   edited, moderated or expire;
//! * [`LocalIndex::seal`] / [`LocalIndex::open`]: the at-rest form, encrypted and
//!   authenticated with XChaCha20-Poly1305 under a DEVICE-LOCAL storage key the SDK keeps
//!   in platform-protected storage (Keychain / Keystore / non-extractable Web Crypto
//!   key). The storage key never leaves the device and is unrelated to MLS keys.
//!
//! This module performs no I/O. Browser / mobile runtime integration is the SDKs' work
//! and is not certified here.

use std::collections::{BTreeMap, BTreeSet};

use aead::{rand_core::RngCore, Aead, KeyInit, OsRng, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use uuid::Uuid;

const SEAL_LABEL: &[u8] = b"cfmeet-local-search-index:v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LocalSearchError {
    #[error("the sealed index failed authentication")]
    Authentication,
    #[error("malformed sealed index")]
    Malformed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    conversation_id: Uuid,
    sequence: i64,
    text: String,
}

/// A local search hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalHit {
    pub message_id: Uuid,
    pub conversation_id: Uuid,
    pub sequence: i64,
    /// A short excerpt around the first matching term (local only).
    pub excerpt: String,
}

/// The device's index of decrypted messages.
#[derive(Default, Clone)]
pub struct LocalIndex {
    entries: BTreeMap<Uuid, Entry>,
    postings: BTreeMap<String, BTreeSet<Uuid>>,
}

impl std::fmt::Debug for LocalIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LocalIndex({} messages)", self.entries.len())
    }
}

fn tokens(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect()
}

impl LocalIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Indexes (or re-indexes, after an edit) one decrypted message.
    pub fn upsert(&mut self, message_id: Uuid, conversation_id: Uuid, sequence: i64, text: &str) {
        self.remove(message_id);
        for t in tokens(text) {
            self.postings.entry(t).or_default().insert(message_id);
        }
        self.entries.insert(
            message_id,
            Entry {
                conversation_id,
                sequence,
                text: text.to_string(),
            },
        );
    }

    /// Forgets a message (deleted, moderated, expired or purged).
    pub fn remove(&mut self, message_id: Uuid) {
        if let Some(e) = self.entries.remove(&message_id) {
            for t in tokens(&e.text) {
                if let Some(set) = self.postings.get_mut(&t) {
                    set.remove(&message_id);
                    if set.is_empty() {
                        self.postings.remove(&t);
                    }
                }
            }
        }
    }

    /// Forgets every message of a conversation (the device left or was removed).
    pub fn remove_conversation(&mut self, conversation_id: Uuid) {
        let ids: Vec<Uuid> = self
            .entries
            .iter()
            .filter(|(_, e)| e.conversation_id == conversation_id)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.remove(id);
        }
    }

    /// Messages containing every query term (a term matches as a word prefix), newest
    /// first, optionally within one conversation.
    pub fn search(&self, query: &str, conversation: Option<Uuid>, limit: usize) -> Vec<LocalHit> {
        let terms = tokens(query);
        if terms.is_empty() {
            return Vec::new();
        }
        let mut result: Option<BTreeSet<Uuid>> = None;
        for term in &terms {
            let matching: BTreeSet<Uuid> = self
                .postings
                .range(term.clone()..)
                .take_while(|(k, _)| k.starts_with(term.as_str()))
                .flat_map(|(_, ids)| ids.iter().copied())
                .collect();
            result = Some(match result {
                None => matching,
                Some(r) => r.intersection(&matching).copied().collect(),
            });
        }
        let first = terms.iter().next().cloned().unwrap_or_default();
        let mut hits: Vec<LocalHit> = result
            .unwrap_or_default()
            .into_iter()
            .filter_map(|id| self.entries.get(&id).map(|e| (id, e)))
            .filter(|(_, e)| conversation.is_none_or(|c| c == e.conversation_id))
            .map(|(id, e)| LocalHit {
                message_id: id,
                conversation_id: e.conversation_id,
                sequence: e.sequence,
                excerpt: excerpt(&e.text, &first),
            })
            .collect();
        hits.sort_by(|a, b| {
            b.sequence
                .cmp(&a.sequence)
                .then(a.message_id.cmp(&b.message_id))
        });
        hits.truncate(limit);
        hits
    }

    /// The encrypted at-rest form under the device-local `storage_key`.
    pub fn seal(&self, storage_key: &[u8; 32]) -> Vec<u8> {
        let mut plain = Vec::new();
        for (id, e) in &self.entries {
            plain.extend_from_slice(id.as_bytes());
            plain.extend_from_slice(e.conversation_id.as_bytes());
            plain.extend_from_slice(&e.sequence.to_be_bytes());
            plain.extend_from_slice(&(e.text.len() as u32).to_be_bytes());
            plain.extend_from_slice(e.text.as_bytes());
        }
        let mut nonce = [0u8; 24];
        OsRng.fill_bytes(&mut nonce);
        let cipher = XChaCha20Poly1305::new(storage_key.into());
        let sealed = cipher
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: &plain,
                    aad: SEAL_LABEL,
                },
            )
            .expect("in-memory encryption cannot fail");
        let mut out = nonce.to_vec();
        out.extend_from_slice(&sealed);
        out
    }

    pub fn open(storage_key: &[u8; 32], sealed: &[u8]) -> Result<Self, LocalSearchError> {
        if sealed.len() < 24 + 16 {
            return Err(LocalSearchError::Malformed);
        }
        let cipher = XChaCha20Poly1305::new(storage_key.into());
        let nonce: [u8; 24] = sealed[..24]
            .try_into()
            .map_err(|_| LocalSearchError::Malformed)?;
        let plain = cipher
            .decrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: &sealed[24..],
                    aad: SEAL_LABEL,
                },
            )
            .map_err(|_| LocalSearchError::Authentication)?;
        let mut index = LocalIndex::new();
        let mut at = 0usize;
        let take = |at: &mut usize, n: usize| -> Result<&[u8], LocalSearchError> {
            let s = plain.get(*at..*at + n).ok_or(LocalSearchError::Malformed)?;
            *at += n;
            Ok(s)
        };
        while at < plain.len() {
            let id =
                Uuid::from_slice(take(&mut at, 16)?).map_err(|_| LocalSearchError::Malformed)?;
            let conv =
                Uuid::from_slice(take(&mut at, 16)?).map_err(|_| LocalSearchError::Malformed)?;
            let seq = i64::from_be_bytes(take(&mut at, 8)?.try_into().unwrap());
            let len = u32::from_be_bytes(take(&mut at, 4)?.try_into().unwrap()) as usize;
            let text = String::from_utf8(take(&mut at, len)?.to_vec())
                .map_err(|_| LocalSearchError::Malformed)?;
            index.upsert(id, conv, seq, &text);
        }
        Ok(index)
    }
}

fn excerpt(text: &str, term: &str) -> String {
    let lower = text.to_lowercase();
    let start = lower.find(term).unwrap_or(0);
    let begin = text
        .char_indices()
        .map(|(i, _)| i)
        .filter(|i| *i <= start)
        .rev()
        .nth(30)
        .unwrap_or(0);
    text[begin..].chars().take(120).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_searches_prunes_and_round_trips_sealed() {
        let mut idx = LocalIndex::new();
        let (c1, c2) = (Uuid::new_v4(), Uuid::new_v4());
        let (m1, m2, m3) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        idx.upsert(m1, c1, 1, "Meet me at the Lagos airport");
        idx.upsert(m2, c1, 2, "Airport pickup confirmed");
        idx.upsert(m3, c2, 1, "Unrelated chatter");
        let hits = idx.search("airp", None, 10);
        assert_eq!(
            hits.iter().map(|h| h.message_id).collect::<Vec<_>>(),
            vec![m2, m1]
        );
        assert_eq!(idx.search("lagos airport", None, 10).len(), 1);
        assert!(idx.search("airport", Some(c2), 10).is_empty());
        // Edit re-indexes; delete prunes.
        idx.upsert(m2, c1, 2, "pickup cancelled");
        assert_eq!(idx.search("airport", None, 10).len(), 1);
        idx.remove(m1);
        assert!(idx.search("airport", None, 10).is_empty());
        let key = [7u8; 32];
        let sealed = idx.seal(&key);
        assert!(!sealed.windows(6).any(|w| w == b"pickup"));
        let back = LocalIndex::open(&key, &sealed).unwrap();
        assert_eq!(back.search("pickup", None, 10).len(), 1);
        assert_eq!(
            LocalIndex::open(&[8u8; 32], &sealed).unwrap_err(),
            LocalSearchError::Authentication
        );
        let mut idx = back;
        idx.remove_conversation(c1);
        assert_eq!(idx.len(), 1);
    }
}
