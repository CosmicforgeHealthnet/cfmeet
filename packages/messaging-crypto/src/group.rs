//! Client-side MLS group operations (feature `client`; Part V E/F, Appendix L).
//!
//! A thin, reviewed wrapper over OpenMLS `MlsGroup` for client SDKs and certification
//! tests: create the group with the conversation's server-assigned group id, add devices
//! from their published KeyPackages (Commit + Welcome), join from a Welcome, remove
//! devices (Commit; the removed leaves cannot derive any later epoch's keys), encrypt an
//! application message under the CF Meet AAD and process what the Delivery Service
//! relays. Every key stays in the device's OpenMLS provider. No primitive is implemented
//! here.

use openmls::prelude::OpenMlsProvider as _;
use openmls::prelude::{
    tls_codec::{Deserialize as _, Serialize as _},
    BasicCredential, GroupId, KeyPackage, KeyPackageIn, LeafNodeIndex, MlsGroup,
    MlsGroupCreateConfig, MlsGroupJoinConfig, MlsMessageBodyIn, MlsMessageIn,
    ProcessedMessageContent, ProtocolMessage, StagedWelcome,
};
use openmls_rust_crypto::OpenMlsRustCrypto;
use uuid::Uuid;

use crate::{
    binding,
    device::Device,
    profile::{MANDATORY_CIPHERSUITE, PROTOCOL_VERSION},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GroupError {
    #[error("malformed MLS input")]
    Malformed,
    #[error("the MLS operation was refused")]
    Refused,
    #[error("no such member device")]
    UnknownMember,
}

/// The device's state of one MLS group.
pub struct ClientGroup {
    group: MlsGroup,
}

impl std::fmt::Debug for ClientGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientGroup")
            .field("epoch", &self.epoch())
            .finish_non_exhaustive()
    }
}

/// What processing one relayed message produced.
#[derive(Debug, PartialEq, Eq)]
pub enum Processed {
    /// A decrypted application message and the AAD MLS authenticated with it.
    Application { aad: Vec<u8>, plaintext: Vec<u8> },
    /// A commit was verified and merged (the group is now at a new epoch).
    CommitMerged { epoch: u64 },
    /// A proposal was received (stored by OpenMLS for a later commit).
    Proposal,
}

/// A Commit and, when devices were added, the Welcome for them.
#[derive(Debug, Clone)]
pub struct CommitOutput {
    pub commit: Vec<u8>,
    pub welcome: Option<Vec<u8>>,
    /// The epoch the commit was created in (what the Delivery Service orders on).
    pub epoch: u64,
}

fn key_package(provider: &OpenMlsRustCrypto, encoded: &[u8]) -> Result<KeyPackage, GroupError> {
    let message =
        MlsMessageIn::tls_deserialize_exact(encoded).map_err(|_| GroupError::Malformed)?;
    let kp: KeyPackageIn = match message.extract() {
        MlsMessageBodyIn::KeyPackage(kp) => kp,
        _ => return Err(GroupError::Malformed),
    };
    kp.validate(provider.crypto(), PROTOCOL_VERSION)
        .map_err(|_| GroupError::Refused)
}

impl Device {
    /// Creates a group with the conversation's server-assigned 32-byte group id (epoch 0,
    /// this device the only member).
    pub fn create_group(&self, group_id: &[u8]) -> Result<ClientGroup, GroupError> {
        let config = MlsGroupCreateConfig::builder()
            .ciphersuite(MANDATORY_CIPHERSUITE)
            .use_ratchet_tree_extension(true)
            .build();
        let group = MlsGroup::new_with_group_id(
            self.provider(),
            self.signer(),
            &config,
            GroupId::from_slice(group_id),
            self.credential(),
        )
        .map_err(|_| GroupError::Refused)?;
        Ok(ClientGroup { group })
    }

    /// Commits the addition of the devices whose KeyPackages are given and merges it
    /// locally. Returns the Commit (for existing members) and the Welcome (for the new).
    pub fn add_devices(
        &self,
        group: &mut ClientGroup,
        key_packages: &[Vec<u8>],
    ) -> Result<CommitOutput, GroupError> {
        let epoch = group.epoch();
        let packages = key_packages
            .iter()
            .map(|k| key_package(self.provider(), k))
            .collect::<Result<Vec<_>, _>>()?;
        let (commit, welcome, _) = group
            .group
            .add_members(self.provider(), self.signer(), &packages)
            .map_err(|_| GroupError::Refused)?;
        group
            .group
            .merge_pending_commit(self.provider())
            .map_err(|_| GroupError::Refused)?;
        Ok(CommitOutput {
            commit: commit
                .tls_serialize_detached()
                .map_err(|_| GroupError::Refused)?,
            welcome: Some(
                welcome
                    .tls_serialize_detached()
                    .map_err(|_| GroupError::Refused)?,
            ),
            epoch,
        })
    }

    /// Commits the removal of every leaf of `devices` and merges it locally.
    pub fn remove_devices(
        &self,
        group: &mut ClientGroup,
        devices: &[Uuid],
    ) -> Result<CommitOutput, GroupError> {
        let epoch = group.epoch();
        let leaves: Vec<LeafNodeIndex> = group
            .group
            .members()
            .filter(|m| {
                BasicCredential::try_from(m.credential.clone())
                    .ok()
                    .and_then(|c| binding::parse_device_credential_identity(c.identity()))
                    .is_some_and(|d| devices.contains(&d))
            })
            .map(|m| m.index)
            .collect();
        if leaves.is_empty() {
            return Err(GroupError::UnknownMember);
        }
        let (commit, _, _) = group
            .group
            .remove_members(self.provider(), self.signer(), &leaves)
            .map_err(|_| GroupError::Refused)?;
        group
            .group
            .merge_pending_commit(self.provider())
            .map_err(|_| GroupError::Refused)?;
        Ok(CommitOutput {
            commit: commit
                .tls_serialize_detached()
                .map_err(|_| GroupError::Refused)?,
            welcome: None,
            epoch,
        })
    }

    /// Joins a group from a Welcome addressed to one of this device's KeyPackages.
    pub fn join(&self, welcome: &[u8]) -> Result<ClientGroup, GroupError> {
        let message =
            MlsMessageIn::tls_deserialize_exact(welcome).map_err(|_| GroupError::Malformed)?;
        let MlsMessageBodyIn::Welcome(welcome) = message.extract() else {
            return Err(GroupError::Malformed);
        };
        let group = StagedWelcome::new_from_welcome(
            self.provider(),
            &MlsGroupJoinConfig::default(),
            welcome,
            None,
        )
        .map_err(|_| GroupError::Refused)?
        .into_group(self.provider())
        .map_err(|_| GroupError::Refused)?;
        Ok(ClientGroup { group })
    }

    /// Encrypts `plaintext` as an application message authenticated with `aad` (the
    /// CF Meet AAD of [`binding::application_message_aad`]).
    pub fn encrypt(
        &self,
        group: &mut ClientGroup,
        aad: &[u8],
        plaintext: &[u8],
    ) -> Result<Vec<u8>, GroupError> {
        group.group.set_aad(aad.to_vec());
        group
            .group
            .create_message(self.provider(), self.signer(), plaintext)
            .map_err(|_| GroupError::Refused)?
            .tls_serialize_detached()
            .map_err(|_| GroupError::Refused)
    }

    /// Processes one message relayed by the Delivery Service: decrypts an application
    /// message or verifies and merges a commit.
    pub fn process(
        &self,
        group: &mut ClientGroup,
        encoded: &[u8],
    ) -> Result<Processed, GroupError> {
        let message =
            MlsMessageIn::tls_deserialize_exact(encoded).map_err(|_| GroupError::Malformed)?;
        let protocol: ProtocolMessage = message
            .try_into_protocol_message()
            .map_err(|_| GroupError::Malformed)?;
        let processed = group
            .group
            .process_message(self.provider(), protocol)
            .map_err(|_| GroupError::Refused)?;
        let aad = processed.aad().to_vec();
        match processed.into_content() {
            ProcessedMessageContent::ApplicationMessage(app) => Ok(Processed::Application {
                aad,
                plaintext: app.into_bytes(),
            }),
            ProcessedMessageContent::StagedCommitMessage(staged) => {
                group
                    .group
                    .merge_staged_commit(self.provider(), *staged)
                    .map_err(|_| GroupError::Refused)?;
                Ok(Processed::CommitMerged {
                    epoch: group.epoch(),
                })
            }
            _ => Ok(Processed::Proposal),
        }
    }
}

impl ClientGroup {
    pub fn epoch(&self) -> u64 {
        self.group.epoch().as_u64()
    }

    pub fn group_id(&self) -> Vec<u8> {
        self.group.group_id().as_slice().to_vec()
    }

    /// The device ids of the group's current leaves (sorted).
    pub fn member_devices(&self) -> Vec<Uuid> {
        let mut out: Vec<Uuid> = self
            .group
            .members()
            .filter_map(|m| {
                BasicCredential::try_from(m.credential)
                    .ok()
                    .and_then(|c| binding::parse_device_credential_identity(c.identity()))
            })
            .collect();
        out.sort();
        out
    }
}
