/// Hand-written `dart:ffi` bindings of the C ABI of `packages/messaging-crypto/ffi`
/// (`include/cf_messaging_crypto.h`, ABI version 1). Private: use `MessagingCrypto`.
library;

import 'dart:ffi';

/// `cfm_buffer`: a library-allocated byte buffer (free with `cfm_buffer_free`).
final class CfmBuffer extends Struct {
  external Pointer<Uint8> ptr;
  @Size()
  external int len;
}

/// `cfm_bytes`: a borrowed input slice.
final class CfmBytes extends Struct {
  external Pointer<Uint8> ptr;
  @Size()
  external int len;
}

/// `cfm_commit_result`.
final class CfmCommitResult extends Struct {
  external CfmBuffer commit;
  external CfmBuffer welcome;
  @Uint8()
  external int hasWelcome;
  @Uint64()
  external int epoch;
}

/// `cfm_process_result`.
final class CfmProcessResult extends Struct {
  @Uint32()
  external int kind;
  external CfmBuffer plaintext;
  @Uint64()
  external int epoch;
}

/// Opaque `cfm_device`.
final class CfmDevice extends Opaque {}

/// Opaque `cfm_encrypted_file`.
final class CfmEncryptedFile extends Opaque {}

typedef CfmIn = Pointer<Uint8>;
typedef CfmOut = Pointer<CfmBuffer>;
typedef CfmList = Pointer<CfmBytes>;
typedef CfmDevicePtr = Pointer<CfmDevice>;
typedef CfmFilePtr = Pointer<CfmEncryptedFile>;

/// Every exported function of the library.
class Bindings {
  Bindings(DynamicLibrary lib)
      : abiVersion = lib.lookupFunction<Uint32 Function(), int Function()>(
            'cfm_abi_version'),
        lastError = lib.lookupFunction<Pointer<Char> Function(),
            Pointer<Char> Function()>('cfm_last_error'),
        bufferFree = lib.lookupFunction<Void Function(CfmBuffer),
            void Function(CfmBuffer)>('cfm_buffer_free'),
        messageAad = lib.lookupFunction<
            Int32 Function(CfmIn, Size, CfmIn, Size, CfmIn, Size, CfmIn, Size,
                CfmIn, Size, CfmIn, Size, CfmOut),
            int Function(CfmIn, int, CfmIn, int, CfmIn, int, CfmIn, int, CfmIn,
                int, CfmIn, int, CfmOut)>('cfm_message_aad'),
        inspectKeyPackage = lib.lookupFunction<
            Int32 Function(CfmIn, Size, Uint64, CfmOut, CfmOut),
            int Function(
                CfmIn, int, int, CfmOut, CfmOut)>('cfm_inspect_key_package'),
        verifyTreeHead = lib.lookupFunction<
            Int32 Function(CfmIn, Size, CfmIn, Size, Uint64, CfmIn, Size, Int64,
                CfmIn, Size, CfmIn),
            int Function(CfmIn, int, CfmIn, int, int, CfmIn, int, int, CfmIn,
                int, CfmIn)>('cfm_verify_tree_head'),
        verifyInclusion = lib.lookupFunction<
            Int32 Function(
                Uint64, Uint64, CfmIn, Size, CfmIn, Size, CfmIn, Size, CfmIn),
            int Function(int, int, CfmIn, int, CfmIn, int, CfmIn, int,
                CfmIn)>('cfm_verify_inclusion'),
        verifyConsistency = lib.lookupFunction<
            Int32 Function(
                Uint64, Uint64, CfmIn, Size, CfmIn, Size, CfmIn, Size, CfmIn),
            int Function(int, int, CfmIn, int, CfmIn, int, CfmIn, int,
                CfmIn)>('cfm_verify_consistency'),
        encryptFile = lib.lookupFunction<
            Int32 Function(
                CfmIn, Size, CfmIn, Size, CfmIn, Size, Pointer<CfmFilePtr>),
            int Function(CfmIn, int, CfmIn, int, CfmIn, int,
                Pointer<CfmFilePtr>)>('cfm_encrypt_file'),
        encryptedFileCiphertext = lib.lookupFunction<
            Int32 Function(CfmFilePtr, CfmOut),
            int Function(CfmFilePtr, CfmOut)>('cfm_encrypted_file_ciphertext'),
        encryptedFileSha256 = lib.lookupFunction<
            Int32 Function(CfmFilePtr, CfmOut),
            int Function(CfmFilePtr, CfmOut)>('cfm_encrypted_file_sha256'),
        encryptedFileSize = lib.lookupFunction<
            Int32 Function(CfmFilePtr, Pointer<Uint64>),
            int Function(
                CfmFilePtr, Pointer<Uint64>)>('cfm_encrypted_file_size'),
        encryptedFileDescriptorFor = lib.lookupFunction<
            Int32 Function(CfmFilePtr, CfmIn, Size, CfmOut),
            int Function(CfmFilePtr, CfmIn, int,
                CfmOut)>('cfm_encrypted_file_descriptor_for'),
        encryptedFileFreePtr =
            lib.lookup<NativeFunction<Void Function(CfmFilePtr)>>(
                'cfm_encrypted_file_free'),
        decryptFile = lib.lookupFunction<
            Int32 Function(
                CfmIn, Size, CfmIn, Size, CfmIn, Size, CfmOut, CfmOut, CfmOut),
            int Function(CfmIn, int, CfmIn, int, CfmIn, int, CfmOut, CfmOut,
                CfmOut)>('cfm_decrypt_file'),
        descriptorInfo = lib.lookupFunction<
            Int32 Function(CfmIn, Size, CfmOut, CfmOut, Pointer<Uint64>),
            int Function(CfmIn, int, CfmOut, CfmOut,
                Pointer<Uint64>)>('cfm_descriptor_info'),
        isDescriptor = lib.lookupFunction<Int32 Function(CfmIn, Size, CfmIn),
            int Function(CfmIn, int, CfmIn)>('cfm_is_descriptor'),
        verifyTransferRequest = lib.lookupFunction<
            Int32 Function(CfmIn, Size, CfmList, Size, CfmIn, Size, Int64,
                CfmIn, Size, CfmIn),
            int Function(CfmIn, int, CfmList, int, CfmIn, int, int, CfmIn, int,
                CfmIn)>('cfm_verify_transfer_request'),
        comparisonCode = lib.lookupFunction<
            Int32 Function(
                CfmList, Size, CfmIn, Size, CfmIn, Size, CfmIn, Size, CfmOut),
            int Function(CfmList, int, CfmIn, int, CfmIn, int, CfmIn, int,
                CfmOut)>('cfm_comparison_code'),
        deviceNew = lib.lookupFunction<
            Int32 Function(CfmIn, Size, Pointer<CfmDevicePtr>),
            int Function(CfmIn, int, Pointer<CfmDevicePtr>)>('cfm_device_new'),
        deviceFromState = lib.lookupFunction<
            Int32 Function(CfmIn, Size, CfmIn, Size, Pointer<CfmDevicePtr>),
            int Function(CfmIn, int, CfmIn, int,
                Pointer<CfmDevicePtr>)>('cfm_device_from_state'),
        deviceExportState = lib.lookupFunction<
            Int32 Function(CfmDevicePtr, CfmOut),
            int Function(CfmDevicePtr, CfmOut)>('cfm_device_export_state'),
        deviceId = lib.lookupFunction<Int32 Function(CfmDevicePtr, CfmOut),
            int Function(CfmDevicePtr, CfmOut)>('cfm_device_id'),
        deviceSignaturePublicKey = lib.lookupFunction<
            Int32 Function(CfmDevicePtr, CfmOut),
            int Function(
                CfmDevicePtr, CfmOut)>('cfm_device_signature_public_key'),
        deviceKeyPackage = lib.lookupFunction<
            Int32 Function(CfmDevicePtr, CfmOut),
            int Function(CfmDevicePtr, CfmOut)>('cfm_device_key_package'),
        deviceRegistrationKeyPackage = lib.lookupFunction<
            Int32 Function(CfmDevicePtr, CfmIn, Size, CfmIn, Size, CfmOut),
            int Function(CfmDevicePtr, CfmIn, int, CfmIn, int,
                CfmOut)>('cfm_device_registration_key_package'),
        deviceHasGroup = lib.lookupFunction<
            Int32 Function(CfmDevicePtr, CfmIn, Size, CfmIn),
            int Function(
                CfmDevicePtr, CfmIn, int, CfmIn)>('cfm_device_has_group'),
        deviceCreateGroup = lib.lookupFunction<
            Int32 Function(CfmDevicePtr, CfmIn, Size, Pointer<Uint64>),
            int Function(CfmDevicePtr, CfmIn, int,
                Pointer<Uint64>)>('cfm_device_create_group'),
        deviceJoin = lib.lookupFunction<
            Int32 Function(
                CfmDevicePtr, CfmIn, Size, CfmIn, Size, Pointer<Uint64>),
            int Function(CfmDevicePtr, CfmIn, int, CfmIn, int,
                Pointer<Uint64>)>('cfm_device_join'),
        deviceAddDevices = lib.lookupFunction<
            Int32 Function(CfmDevicePtr, CfmIn, Size, CfmIn, Size,
                Pointer<CfmCommitResult>),
            int Function(CfmDevicePtr, CfmIn, int, CfmIn, int,
                Pointer<CfmCommitResult>)>('cfm_device_add_devices'),
        deviceRemoveDevices = lib.lookupFunction<
            Int32 Function(CfmDevicePtr, CfmIn, Size, CfmList, Size,
                Pointer<CfmCommitResult>),
            int Function(CfmDevicePtr, CfmIn, int, CfmList, int,
                Pointer<CfmCommitResult>)>('cfm_device_remove_devices'),
        deviceEncrypt = lib.lookupFunction<
            Int32 Function(
                CfmDevicePtr, CfmIn, Size, CfmIn, Size, CfmIn, Size, CfmOut),
            int Function(CfmDevicePtr, CfmIn, int, CfmIn, int, CfmIn, int,
                CfmOut)>('cfm_device_encrypt'),
        deviceProcess = lib.lookupFunction<
            Int32 Function(CfmDevicePtr, CfmIn, Size, CfmIn, Size, CfmIn, Size,
                Uint8, Pointer<CfmProcessResult>),
            int Function(CfmDevicePtr, CfmIn, int, CfmIn, int, CfmIn, int, int,
                Pointer<CfmProcessResult>)>('cfm_device_process'),
        deviceEpoch = lib.lookupFunction<
            Int32 Function(CfmDevicePtr, CfmIn, Size, Pointer<Uint64>),
            int Function(
                CfmDevicePtr, CfmIn, int, Pointer<Uint64>)>('cfm_device_epoch'),
        deviceMembers = lib.lookupFunction<
            Int32 Function(CfmDevicePtr, CfmIn, Size, CfmOut),
            int Function(
                CfmDevicePtr, CfmIn, int, CfmOut)>('cfm_device_members'),
        deviceBeginTransfer = lib.lookupFunction<
            Int32 Function(CfmDevicePtr, CfmOut),
            int Function(CfmDevicePtr, CfmOut)>('cfm_device_begin_transfer'),
        deviceSignTransferRequest = lib.lookupFunction<
            Int32 Function(CfmDevicePtr, CfmList, Size, Int64, CfmOut),
            int Function(CfmDevicePtr, CfmList, int, int,
                CfmOut)>('cfm_device_sign_transfer_request'),
        deviceSealHistory = lib.lookupFunction<
            Int32 Function(
                CfmDevicePtr, CfmList, Size, CfmIn, Size, CfmIn, Size, CfmOut),
            int Function(CfmDevicePtr, CfmList, int, CfmIn, int, CfmIn, int,
                CfmOut)>('cfm_device_seal_history'),
        deviceOpenHistory = lib.lookupFunction<
            Int32 Function(
                CfmDevicePtr, CfmIn, Size, CfmList, Size, CfmIn, Size, CfmOut),
            int Function(CfmDevicePtr, CfmIn, int, CfmList, int, CfmIn, int,
                CfmOut)>('cfm_device_open_history'),
        deviceFreePtr = lib.lookup<NativeFunction<Void Function(CfmDevicePtr)>>(
            'cfm_device_free');

  final int Function() abiVersion;
  final Pointer<Char> Function() lastError;
  final void Function(CfmBuffer) bufferFree;
  final int Function(CfmIn, int, CfmIn, int, CfmIn, int, CfmIn, int, CfmIn, int,
      CfmIn, int, CfmOut) messageAad;
  final int Function(CfmIn, int, int, CfmOut, CfmOut) inspectKeyPackage;
  final int Function(
          CfmIn, int, CfmIn, int, int, CfmIn, int, int, CfmIn, int, CfmIn)
      verifyTreeHead;
  final int Function(int, int, CfmIn, int, CfmIn, int, CfmIn, int, CfmIn)
      verifyInclusion;
  final int Function(int, int, CfmIn, int, CfmIn, int, CfmIn, int, CfmIn)
      verifyConsistency;
  final int Function(CfmIn, int, CfmIn, int, CfmIn, int, Pointer<CfmFilePtr>)
      encryptFile;
  final int Function(CfmFilePtr, CfmOut) encryptedFileCiphertext;
  final int Function(CfmFilePtr, CfmOut) encryptedFileSha256;
  final int Function(CfmFilePtr, Pointer<Uint64>) encryptedFileSize;
  final int Function(CfmFilePtr, CfmIn, int, CfmOut) encryptedFileDescriptorFor;
  final Pointer<NativeFunction<Void Function(CfmFilePtr)>> encryptedFileFreePtr;
  final int Function(CfmIn, int, CfmIn, int, CfmIn, int, CfmOut, CfmOut, CfmOut)
      decryptFile;
  final int Function(CfmIn, int, CfmOut, CfmOut, Pointer<Uint64>)
      descriptorInfo;
  final int Function(CfmIn, int, CfmIn) isDescriptor;
  final int Function(
          CfmIn, int, CfmList, int, CfmIn, int, int, CfmIn, int, CfmIn)
      verifyTransferRequest;
  final int Function(CfmList, int, CfmIn, int, CfmIn, int, CfmIn, int, CfmOut)
      comparisonCode;
  final int Function(CfmIn, int, Pointer<CfmDevicePtr>) deviceNew;
  final int Function(CfmIn, int, CfmIn, int, Pointer<CfmDevicePtr>)
      deviceFromState;
  final int Function(CfmDevicePtr, CfmOut) deviceExportState;
  final int Function(CfmDevicePtr, CfmOut) deviceId;
  final int Function(CfmDevicePtr, CfmOut) deviceSignaturePublicKey;
  final int Function(CfmDevicePtr, CfmOut) deviceKeyPackage;
  final int Function(CfmDevicePtr, CfmIn, int, CfmIn, int, CfmOut)
      deviceRegistrationKeyPackage;
  final int Function(CfmDevicePtr, CfmIn, int, CfmIn) deviceHasGroup;
  final int Function(CfmDevicePtr, CfmIn, int, Pointer<Uint64>)
      deviceCreateGroup;
  final int Function(CfmDevicePtr, CfmIn, int, CfmIn, int, Pointer<Uint64>)
      deviceJoin;
  final int Function(
          CfmDevicePtr, CfmIn, int, CfmIn, int, Pointer<CfmCommitResult>)
      deviceAddDevices;
  final int Function(
          CfmDevicePtr, CfmIn, int, CfmList, int, Pointer<CfmCommitResult>)
      deviceRemoveDevices;
  final int Function(CfmDevicePtr, CfmIn, int, CfmIn, int, CfmIn, int, CfmOut)
      deviceEncrypt;
  final int Function(CfmDevicePtr, CfmIn, int, CfmIn, int, CfmIn, int, int,
      Pointer<CfmProcessResult>) deviceProcess;
  final int Function(CfmDevicePtr, CfmIn, int, Pointer<Uint64>) deviceEpoch;
  final int Function(CfmDevicePtr, CfmIn, int, CfmOut) deviceMembers;
  final int Function(CfmDevicePtr, CfmOut) deviceBeginTransfer;
  final int Function(CfmDevicePtr, CfmList, int, int, CfmOut)
      deviceSignTransferRequest;
  final int Function(CfmDevicePtr, CfmList, int, CfmIn, int, CfmIn, int, CfmOut)
      deviceSealHistory;
  final int Function(CfmDevicePtr, CfmIn, int, CfmList, int, CfmIn, int, CfmOut)
      deviceOpenHistory;
  final Pointer<NativeFunction<Void Function(CfmDevicePtr)>> deviceFreePtr;
}
