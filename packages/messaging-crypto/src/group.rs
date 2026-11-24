//! Client-side MLS group operations (feature `client`; Part V E/F, Appendix L).
//!
//! A thin, reviewed wrapper over OpenMLS `MlsGroup` for client SDKs and certification
//! tests: create the group with the conversation's server-assigned group id, add devices
//! from their published KeyPackages (Commit + Welcome), join from a Welcome, remove
//! devices (Commit; the removed leaves cannot derive any later epoch's keys), encrypt an
//! application message under the CF Meet AAD and process what the Delivery Service
//! relays. Every key stays in the device's OpenMLS provider. No primitive is implemented
//! here.

use openmls::framing::errors::{MessageDecryptionError, SecretTreeError};
use openmls::prelude::OpenMlsProvider as _;
use openmls::prelude::{
    tls_codec::{Deserialize as _, Serialize as _},
    BasicCredential, GroupId, KeyPackage, KeyPackageIn, LeafNodeIndex, MlsGroup,
    MlsGroupCreateConfig, MlsGroupJoinConfig, MlsMessageBodyIn, MlsMessageIn, ProcessMessageError,
    ProcessedMessageContent, ProtocolMessage, Sender, StagedWelcome, ValidationError,
};
use uuid::Uuid;

use crate::{
    binding,
    device::Device,
    profile::{MANDATORY_CIPHERSUITE, PROTOCOL_VERSION},
    provider::CfMlsProvider,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GroupError {
    #[error("malformed MLS input")]
    Malformed,
    #[error("the MLS operation was refused")]
    Refused,
    /// Allowlisted public processing diagnostic, never formatted crypto internals.
    #[error("{0}")]
    Processing(&'static str),
    #[error("no such member device")]
    UnknownMember,
    /// An external commit (RFC 9420 §12.4.3.2) whose joining device the caller has not
    /// (yet) verified as a registered device of a member: nothing was merged.
    #[error("the joining device is not verified")]
    JoinerNotAllowed(Uuid),
}

impl GroupError {
    pub fn public_code(&self) -> &'static str {
        match self {
            Self::Malformed => "MALFORMED_MLS_MESSAGE",
            Self::Processing(code) => code,
            Self::UnknownMember => "MEMBERSHIP_UNKNOWN",
            Self::JoinerNotAllowed(_) => "JOINER_UNVERIFIED",
            Self::Refused => "MESSAGE_REFUSED",
        }
    }
}

fn processing_error<E>(error: ProcessMessageError<E>) -> GroupError {
    use MessageDecryptionError as D;
    use ProcessMessageError as P;
    use ValidationError as V;
    let code = match error {
        P::ValidationError(V::WrongEpoch) => "EPOCH_MISMATCH",
        P::ValidationError(V::NoPastEpochData) => "HISTORICAL_EPOCH_UNAVAILABLE",
        P::ValidationError(V::WrongGroupId) => "GROUP_ID_MISMATCH",
        P::ValidationError(V::UnknownMember | V::NonMemberApplicationMessage) => {
            "MEMBERSHIP_UNKNOWN"
        }
        P::ValidationError(V::UnableToDecrypt(D::SecretTreeError(
            SecretTreeError::SecretReuseError,
        ))) => "REPLAY_REJECTED",
        P::ValidationError(V::UnableToDecrypt(D::SecretTreeError(
            SecretTreeError::TooDistantInThePast,
        ))) => "RATCHET_STATE_UNAVAILABLE",
        P::ValidationError(V::UnableToDecrypt(D::GenerationOutOfBound)) => {
            "RATCHET_GENERATION_UNAVAILABLE"
        }
        P::ValidationError(
            V::UnableToDecrypt(D::AeadError) | V::InvalidSignature | V::InvalidMembershipTag,
        ) => "MESSAGE_AUTHENTICATION_FAILED",
        P::StorageError(_) => "CRYPTO_STORAGE_ERROR",
        P::GroupStateError(_) => "GROUP_STATE_REFUSED",
        P::InvalidCommit(_) => "COMMIT_REFUSED",
        _ => "MESSAGE_VALIDATION_FAILED",
    };
    GroupError::Processing(code)
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
    /// A verified external commit (a device joined by itself, RFC 9420 §12.4.3.2) was
    /// merged. The caller's joiner policy accepted `device_id` with `signature_key`.
    ExternalJoin {
        epoch: u64,
        device_id: Uuid,
        signature_key: Vec<u8>,
    },
}

/// The joining device of a staged external commit, after the checks the application must
/// make (RFC 9420 §12.4.3.2; OpenMLS leaves them to the application): the joiner's leaf
/// carries a CF Meet device credential, no pre-shared key is injected and a Remove
/// proposal may only remove a leaf of the SAME device (a resync). An external commit can
/// therefore never evict another device.
fn external_joiner(
    group: &MlsGroup,
    staged: &openmls::prelude::StagedCommit,
) -> Result<(Uuid, Vec<u8>), GroupError> {
    let leaf = staged.update_path_leaf_node().ok_or(GroupError::Refused)?;
    let credential =
        BasicCredential::try_from(leaf.credential().clone()).map_err(|_| GroupError::Refused)?;
    let device = binding::parse_device_credential_identity(credential.identity())
        .ok_or(GroupError::Refused)?;
    if staged.psk_proposals().next().is_some() || staged.add_proposals().next().is_some() {
        return Err(GroupError::Refused);
    }
    for remove in staged.remove_proposals() {
        let removed = group
            .member_at(remove.remove_proposal().removed())
            .ok_or(GroupError::Refused)?;
        let same = BasicCredential::try_from(removed.credential)
            .ok()
            .and_then(|c| binding::parse_device_credential_identity(c.identity()));
        if same != Some(device) {
            return Err(GroupError::Refused);
        }
    }
    Ok((device, leaf.signature_key().as_slice().to_vec()))
}

/// A Commit and, when devices were added, the Welcome for them.
#[derive(Debug, Clone)]
pub struct CommitOutput {
    pub commit: Vec<u8>,
    pub welcome: Option<Vec<u8>>,
    /// The epoch the commit was created in (what the Delivery Service orders on).
    pub epoch: u64,
}

fn key_package(provider: &CfMlsProvider, encoded: &[u8]) -> Result<KeyPackage, GroupError> {
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

    /// Prepares an addition without advancing the local epoch (RFC 9420 section 14).
    /// Submit the Commit first; after canonical acceptance call `accept_pending_commit`.
    /// On refusal call `discard_pending_commit`. Do not publish the Welcome first.
    pub fn prepare_add_devices(
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

    /// Prepares removal without advancing the epoch; accept/discard after the server verdict.
    pub fn prepare_remove_devices(
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
        Ok(CommitOutput {
            commit: commit
                .tls_serialize_detached()
                .map_err(|_| GroupError::Refused)?,
            welcome: None,
            epoch,
        })
    }

    /// Commits a fresh leaf for this device (new HPKE and path secrets; same signature
    /// key), without advancing the epoch. Accept/discard after the server verdict.
    pub fn prepare_update_keys(&self, group: &mut ClientGroup) -> Result<CommitOutput, GroupError> {
        let epoch = group.epoch();
        let bundle = group
            .group
            .self_update(
                self.provider(),
                self.signer(),
                openmls::treesync::LeafNodeParameters::default(),
            )
            .map_err(|_| GroupError::Refused)?;
        Ok(CommitOutput {
            commit: bundle
                .commit()
                .tls_serialize_detached()
                .map_err(|_| GroupError::Refused)?,
            welcome: None,
            epoch,
        })
    }

    /// Finalizes the locally pending Commit ONLY after matching canonical acceptance.
    pub fn accept_pending_commit(&self, group: &mut ClientGroup) -> Result<(), GroupError> {
        if group.group.pending_commit().is_none() {
            return Err(GroupError::Refused);
        }
        group
            .group
            .merge_pending_commit(self.provider())
            .map_err(|_| GroupError::Refused)
    }

    /// Discards a refused/superseded local proposal without advancing the epoch.
    /// Fetch/process the winning canonical Commit before preparing another change.
    pub fn discard_pending_commit(&self, group: &mut ClientGroup) -> Result<(), GroupError> {
        group
            .group
            .clear_pending_commit(self.provider().storage())
            .map_err(|_| GroupError::Refused)
    }

    /// Immediate local-simulation compatibility helper. Network clients must use
    /// `prepare_add_devices` followed by canonical acceptance, not this convenience API.
    pub fn add_devices(
        &self,
        group: &mut ClientGroup,
        packages: &[Vec<u8>],
    ) -> Result<CommitOutput, GroupError> {
        let out = self.prepare_add_devices(group, packages)?;
        self.accept_pending_commit(group)?;
        Ok(out)
    }
    /// Immediate local-simulation compatibility helper; live clients use the staged API.
    pub fn remove_devices(
        &self,
        group: &mut ClientGroup,
        devices: &[Uuid],
    ) -> Result<CommitOutput, GroupError> {
        let out = self.prepare_remove_devices(group, devices)?;
        self.accept_pending_commit(group)?;
        Ok(out)
    }
    /// Immediate local-simulation compatibility helper; live clients use the staged API.
    pub fn update_keys(&self, group: &mut ClientGroup) -> Result<CommitOutput, GroupError> {
        let out = self.prepare_update_keys(group)?;
        self.accept_pending_commit(group)?;
        Ok(out)
    }

    /// The device's state of a group it created or joined earlier, read back from its
    /// own storage (after [`Device::import_state`]). `None`: the device has no such group.
    pub fn load_group(&self, group_id: &[u8]) -> Result<Option<ClientGroup>, GroupError> {
        MlsGroup::load(self.provider().storage(), &GroupId::from_slice(group_id))
            .map(|g| g.map(|group| ClientGroup { group }))
            .map_err(|_| GroupError::Malformed)
    }

    /// Joins a group from a Welcome addressed to one of this device's KeyPackages.
    pub fn join(&self, welcome: &[u8]) -> Result<ClientGroup, GroupError> {
        let message =
            MlsMessageIn::tls_deserialize_exact(welcome).map_err(|_| GroupError::Malformed)?;
        let MlsMessageBodyIn::Welcome(welcome) = message.extract() else {
            return Err(GroupError::Malformed);
        };
        // The same ratchet-tree policy as `create_group`: Welcomes this member sends later
        // must carry the tree, or the devices it adds can never join.
        let config = MlsGroupJoinConfig::builder()
            .use_ratchet_tree_extension(true)
            .build();
        let group = StagedWelcome::new_from_welcome(self.provider(), &config, welcome, None)
            .map_err(|_| GroupError::Refused)?
            .into_group(self.provider())
            .map_err(|_| GroupError::Refused)?;
        Ok(ClientGroup { group })
    }

    /// The group's current GroupInfo (RFC 9420 §12.4.3), signed by this device, with the
    /// ratchet tree and the `external_pub` extension: what another device of a member needs
    /// to join by an external commit. It holds PUBLIC material only (tree public keys,
    /// credentials, the group context and the external HPKE public key).
    pub fn export_group_info(&self, group: &ClientGroup) -> Result<Vec<u8>, GroupError> {
        group
            .group
            .export_group_info(self.provider().crypto(), self.signer(), true)
            .map_err(|_| GroupError::Refused)?
            .tls_serialize_detached()
            .map_err(|_| GroupError::Refused)
    }

    /// Joins a group by itself from its GroupInfo (an external commit, RFC 9420
    /// §12.4.3.2): no other member has to be online. Returns the group, already at the new
    /// epoch, and the commit to submit (created in the GroupInfo's epoch). If the Delivery
    /// Service refuses it (another commit won the epoch), discard the returned group and
    /// try again from the newer GroupInfo. Members accept the commit only for a device
    /// their joiner policy verified ([`Device::process_with_joiners`]).
    pub fn join_by_external_commit(
        &self,
        group_info: &[u8],
    ) -> Result<(ClientGroup, CommitOutput), GroupError> {
        let message =
            MlsMessageIn::tls_deserialize_exact(group_info).map_err(|_| GroupError::Malformed)?;
        let MlsMessageBodyIn::GroupInfo(info) = message.extract() else {
            return Err(GroupError::Malformed);
        };
        if info.ciphersuite() != MANDATORY_CIPHERSUITE {
            return Err(GroupError::Refused);
        }
        let epoch = info.epoch().as_u64();
        let config = MlsGroupJoinConfig::builder()
            .use_ratchet_tree_extension(true)
            .build();
        let (group, bundle) = MlsGroup::external_commit_builder()
            .with_config(config)
            .build_group(self.provider(), info, self.credential())
            .map_err(|_| GroupError::Refused)?
            .load_psks(self.provider().storage())
            .map_err(|_| GroupError::Refused)?
            .build(
                self.provider().rand(),
                self.provider().crypto(),
                self.signer(),
                |_| true,
            )
            .map_err(|_| GroupError::Refused)?
            .finalize(self.provider())
            .map_err(|_| GroupError::Refused)?;
        let commit = bundle
            .commit()
            .tls_serialize_detached()
            .map_err(|_| GroupError::Refused)?;
        Ok((
            ClientGroup { group },
            CommitOutput {
                commit,
                welcome: None,
                epoch,
            },
        ))
    }

    /// Encrypts `plaintext` as an application message authenticated with `aad` (the
    /// CF Meet AAD of [`binding::application_message_aad`]).
    pub fn encrypt(
        &self,
        group: &mut ClientGroup,
        aad: &[u8],
        plaintext: &[u8],
    ) -> Result<Vec<u8>, GroupError> {
        if group.group.pending_commit().is_some() {
            return Err(GroupError::Refused);
        }
        group.group.set_aad(aad.to_vec());
        group
            .group
            .create_message(self.provider(), self.signer(), plaintext)
            .map_err(|_| GroupError::Refused)?
            .tls_serialize_detached()
            .map_err(|_| GroupError::Refused)
    }

    /// Processes one message relayed by the Delivery Service: decrypts an application
    /// message or verifies and merges a commit. An external commit passes the protocol
    /// checks of [`external_joiner`] and is merged; callers that can verify the joining
    /// device against the device directory use [`Device::process_with_joiners`].
    pub fn process(
        &self,
        group: &mut ClientGroup,
        encoded: &[u8],
    ) -> Result<Processed, GroupError> {
        self.process_with_joiners(group, encoded, |_: Uuid, _: &[u8]| true)
    }

    /// [`Device::process`] with a joiner policy: an external commit is merged only when
    /// `joiner_allowed(device_id, signature_key)` accepts its joining device (a registered,
    /// ACTIVE device of a principal that is a member, with exactly that key). Otherwise
    /// nothing is merged and [`GroupError::JoinerNotAllowed`] names the device, so the
    /// caller can verify it and process the commit again.
    pub fn process_with_joiners(
        &self,
        group: &mut ClientGroup,
        encoded: &[u8],
        joiner_allowed: impl Fn(Uuid, &[u8]) -> bool,
    ) -> Result<Processed, GroupError> {
        let message =
            MlsMessageIn::tls_deserialize_exact(encoded).map_err(|_| GroupError::Malformed)?;
        let protocol: ProtocolMessage = message
            .try_into_protocol_message()
            .map_err(|_| GroupError::Malformed)?;
        let processed = group
            .group
            .process_message(self.provider(), protocol)
            .map_err(processing_error)?;
        let aad = processed.aad().to_vec();
        let external = matches!(processed.sender(), Sender::NewMemberCommit);
        if matches!(
            processed.content(),
            ProcessedMessageContent::ApplicationMessage(_)
        ) && aad.starts_with(b"cfmeet-messaging-aad:")
        {
            // MLS authenticates the leaf credential, not an arbitrary sender id
            // a malicious group member places in application AAD. Bind both before
            // returning any application content to the caller.
            let claimed =
                binding::parse_application_message_aad(&aad).ok_or(GroupError::Refused)?;
            let credential = BasicCredential::try_from(processed.credential().clone())
                .map_err(|_| GroupError::Refused)?;
            if binding::parse_device_credential_identity(credential.identity())
                != Some(claimed.sender_device_id)
            {
                return Err(GroupError::Processing("SENDER_BINDING_MISMATCH"));
            }
        }
        match processed.into_content() {
            ProcessedMessageContent::ApplicationMessage(app) => Ok(Processed::Application {
                aad,
                plaintext: app.into_bytes(),
            }),
            ProcessedMessageContent::StagedCommitMessage(staged) if external => {
                let (device_id, signature_key) = external_joiner(&group.group, &staged)?;
                if !joiner_allowed(device_id, &signature_key) {
                    return Err(GroupError::JoinerNotAllowed(device_id));
                }
                group
                    .group
                    .merge_staged_commit(self.provider(), *staged)
                    .map_err(|_| GroupError::Refused)?;
                Ok(Processed::ExternalJoin {
                    epoch: group.epoch(),
                    device_id,
                    signature_key,
                })
            }
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
