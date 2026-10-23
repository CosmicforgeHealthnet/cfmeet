/// The CF Meet Messaging E2EE client core (MLS 1.0 via OpenMLS, attachments, key
/// transparency, history transfer) for native Dart / Flutter, over the C ABI of
/// `packages/messaging-crypto/ffi` (`dart:ffi`).
///
/// No cryptography is implemented in Dart: every operation runs in the native core.
/// Private keys and MLS secrets never leave it, except as the one opaque device state
/// ([MessagingDevice.exportState]) which the application must store only encrypted under
/// a platform-protected key (Keychain / Keystore).
library;

import 'dart:convert';
import 'dart:ffi';
import 'dart:io' show Platform;
import 'dart:typed_data';

import 'package:ffi/ffi.dart';

import 'bindings.dart';

/// The C ABI version this binding speaks.
const int messagingCryptoAbiVersion = 1;

/// A refusal of the native core: [code] is a stable identifier (`MESSAGE_REFUSED`,
/// `AAD_MISMATCH`, `BAD_UUID`, ...). Never contains key material or plaintext.
class MessagingCryptoException implements Exception {
  const MessagingCryptoException(this.code, {this.status = 1});

  final String code;

  /// The C status: 1 refused, 2 invalid argument, 3 internal failure (handle poisoned).
  final int status;

  @override
  String toString() => 'MessagingCryptoException($code)';
}

/// The library could not be loaded or is of another ABI version.
class MessagingCryptoLoadError extends Error {
  MessagingCryptoLoadError(this.message);

  final String message;

  @override
  String toString() => 'MessagingCryptoLoadError: $message';
}

/// What a validated KeyPackage says about its device.
class KeyPackageInfo {
  const KeyPackageInfo(this.deviceId, this.signatureKey);

  final String deviceId;

  /// Ed25519 signature public key (32 bytes).
  final Uint8List signatureKey;
}

/// A Commit and, when devices were added, the Welcome for them.
class CommitResult {
  const CommitResult(this.commit, this.welcome, this.epoch);

  final Uint8List commit;
  final Uint8List? welcome;

  /// The epoch the commit was created in (what the Delivery Service orders on).
  final int epoch;
}

enum ProcessedKind { application, commit, proposal }

/// What processing one relayed MLS message produced.
class ProcessResult {
  const ProcessResult(this.kind, this.plaintext, this.epoch);

  final ProcessedKind kind;

  /// The decrypted application message (empty for a commit or a proposal).
  final Uint8List plaintext;
  final int epoch;
}

class DecryptedFile {
  const DecryptedFile(this.plaintext, this.mediaType, this.fileName);

  final Uint8List plaintext;
  final String mediaType;
  final String fileName;
}

class DescriptorInfo {
  const DescriptorInfo(this.mediaType, this.fileName, this.size);

  final String mediaType;
  final String fileName;

  /// Plaintext size in bytes.
  final int size;
}

/// The seven identifiers a history transfer is bound to.
class TransferContext {
  const TransferContext({
    required this.transferId,
    required this.tenantId,
    required this.applicationId,
    required this.environmentId,
    required this.principalId,
    required this.sourceDeviceId,
    required this.targetDeviceId,
  });

  final String transferId;
  final String tenantId;
  final String applicationId;
  final String environmentId;
  final String principalId;
  final String sourceDeviceId;
  final String targetDeviceId;

  List<String> get ids => [
        transferId,
        tenantId,
        applicationId,
        environmentId,
        principalId,
        sourceDeviceId,
        targetDeviceId,
      ];
}

/// Packs byte strings as `[u32 big-endian length][bytes]...` (the KeyPackage list format).
Uint8List packByteStrings(List<List<int>> items) {
  final b = BytesBuilder(copy: false);
  for (final i in items) {
    b.add((ByteData(4)..setUint32(0, i.length)).buffer.asUint8List());
    b.add(i);
  }
  return b.takeBytes();
}

/// The inverse of [packByteStrings].
List<Uint8List> unpackByteStrings(Uint8List packed) {
  final out = <Uint8List>[];
  var i = 0;
  final view = ByteData.sublistView(packed);
  while (i < packed.length) {
    if (packed.length - i < 4) throw const FormatException('bad packing');
    final n = view.getUint32(i);
    i += 4;
    if (packed.length - i < n) throw const FormatException('bad packing');
    out.add(Uint8List.sublistView(packed, i, i + n));
    i += n;
  }
  return out;
}

/// The default file name of the native library on this platform.
String defaultMessagingCryptoLibraryName() {
  if (Platform.isMacOS || Platform.isIOS) {
    return 'libcf_messaging_crypto_ffi.dylib';
  }
  if (Platform.isWindows) return 'cf_messaging_crypto_ffi.dll';
  return 'libcf_messaging_crypto_ffi.so';
}

/// Scoped native allocations; secret inputs are zeroed before they are freed.
class _Scope {
  final List<void Function()> _release = [];

  T add<T>(T value, void Function() release) {
    _release.add(release);
    return value;
  }

  /// Copies [bytes] into native memory (NULL for empty).
  Pointer<Uint8> bytes(List<int> bytes) {
    if (bytes.isEmpty) return nullptr;
    final p = calloc<Uint8>(bytes.length);
    p.asTypedList(bytes.length).setAll(0, bytes);
    final n = bytes.length;
    return add(p, () {
      p.asTypedList(n).fillRange(0, n, 0);
      calloc.free(p);
    });
  }

  Pointer<T> track<T extends NativeType>(Pointer<T> p) =>
      add(p, () => calloc.free(p));

  Pointer<CfmBuffer> buffer() => track(calloc<CfmBuffer>());

  /// A native array of [CfmBytes] over UTF-8 strings.
  Pointer<CfmBytes> strings(List<String> values) {
    if (values.isEmpty) return nullptr;
    final arr = calloc<CfmBytes>(values.length);
    add(arr, () => calloc.free(arr));
    for (var i = 0; i < values.length; i++) {
      final u = utf8.encode(values[i]);
      arr[i]
        ..ptr = bytes(u)
        ..len = u.length;
    }
    return arr;
  }

  void release() {
    for (final r in _release.reversed) {
      r();
    }
    _release.clear();
  }
}

/// The loaded native core.
class MessagingCrypto {
  MessagingCrypto._(this._b) {
    _deviceFinalizer = NativeFinalizer(_b.deviceFreePtr.cast());
    _fileFinalizer = NativeFinalizer(_b.encryptedFileFreePtr.cast());
  }

  /// Loads the library from [path], or the platform's default name from the library
  /// search path ([defaultMessagingCryptoLibraryName]); statically linked on iOS when
  /// [path] is null and [useProcess] is true. Refuses a library of another ABI version.
  factory MessagingCrypto.open({String? path, bool useProcess = false}) {
    final DynamicLibrary lib;
    try {
      lib = useProcess
          ? DynamicLibrary.process()
          : DynamicLibrary.open(path ?? defaultMessagingCryptoLibraryName());
    } on ArgumentError catch (e) {
      throw MessagingCryptoLoadError(
          'cannot load the native library: ${e.message}');
    }
    final b = Bindings(lib);
    final v = b.abiVersion();
    if (v != messagingCryptoAbiVersion) {
      throw MessagingCryptoLoadError(
          'native library ABI version $v, this binding needs $messagingCryptoAbiVersion');
    }
    return MessagingCrypto._(b);
  }

  final Bindings _b;
  late final NativeFinalizer _deviceFinalizer;
  late final NativeFinalizer _fileFinalizer;

  int get abiVersion => _b.abiVersion();

  T _call<T>(T Function(_Scope s) body) {
    final s = _Scope();
    try {
      return body(s);
    } finally {
      s.release();
    }
  }

  void _check(int status) {
    if (status == 0) return;
    // Read on the same thread, synchronously after the failing call.
    final code = _b.lastError().cast<Utf8>().toDartString();
    throw MessagingCryptoException(code.isEmpty ? 'UNKNOWN' : code,
        status: status);
  }

  /// Copies and frees (zeroizing) a library buffer.
  Uint8List _take(CfmBuffer buf) {
    if (buf.len == 0 || buf.ptr == nullptr) return Uint8List(0);
    final out = Uint8List.fromList(buf.ptr.asTypedList(buf.len));
    _b.bufferFree(buf);
    return out;
  }

  String _takeString(CfmBuffer buf) => utf8.decode(_take(buf));

  Uint8List _bytesOut(_Scope s, int Function(Pointer<CfmBuffer> out) call) {
    final out = s.buffer();
    _check(call(out));
    return _take(out.ref);
  }

  bool _flag(_Scope s, int Function(Pointer<Uint8> out) call) {
    final out = s.track(calloc<Uint8>());
    _check(call(out));
    return out.value != 0;
  }

  /// The CF Meet AAD of one application message.
  Uint8List messageAad({
    required String tenantId,
    required String applicationId,
    required String environmentId,
    required String conversationId,
    required String clientMessageId,
    required String senderDeviceId,
  }) =>
      _call((s) {
        final ids = [
          tenantId,
          applicationId,
          environmentId,
          conversationId,
          clientMessageId,
          senderDeviceId
        ].map(utf8.encode).toList();
        return _bytesOut(
            s,
            (out) => _b.messageAad(
                s.bytes(ids[0]),
                ids[0].length,
                s.bytes(ids[1]),
                ids[1].length,
                s.bytes(ids[2]),
                ids[2].length,
                s.bytes(ids[3]),
                ids[3].length,
                s.bytes(ids[4]),
                ids[4].length,
                s.bytes(ids[5]),
                ids[5].length,
                out));
      });

  /// Fully validates a published KeyPackage (signature, suite, lifetime at [now]).
  KeyPackageInfo inspectKeyPackage(List<int> keyPackage, {DateTime? now}) =>
      _call((s) {
        final id = s.buffer();
        final key = s.buffer();
        final t = (now ?? DateTime.now()).millisecondsSinceEpoch ~/ 1000;
        _check(_b.inspectKeyPackage(
            s.bytes(keyPackage), keyPackage.length, t, id, key));
        return KeyPackageInfo(_takeString(id.ref), _take(key.ref));
      });

  /// Verifies a key-transparency tree head signed by the pinned log key.
  bool verifyTreeHead({
    required List<int> logPublicKey,
    required String logId,
    required int treeSize,
    required List<int> rootHash,
    required int timestampMicros,
    required List<int> signature,
  }) =>
      _call((s) {
        final id = utf8.encode(logId);
        return _flag(
            s,
            (out) => _b.verifyTreeHead(
                s.bytes(logPublicKey),
                logPublicKey.length,
                s.bytes(id),
                id.length,
                treeSize,
                s.bytes(rootHash),
                rootHash.length,
                timestampMicros,
                s.bytes(signature),
                signature.length,
                out));
      });

  /// Verifies that [leafData] is leaf [index] of the tree of [size] with [root]
  /// ([proof]: concatenated 32-byte hashes).
  bool verifyInclusion(
          {required int index,
          required int size,
          required List<int> leafData,
          required List<int> proof,
          required List<int> root}) =>
      _call((s) => _flag(
          s,
          (out) => _b.verifyInclusion(
              index,
              size,
              s.bytes(leafData),
              leafData.length,
              s.bytes(proof),
              proof.length,
              s.bytes(root),
              root.length,
              out)));

  /// Verifies that the tree of [newSize] / [newRoot] extends [oldSize] / [oldRoot].
  bool verifyConsistency(
          {required int oldSize,
          required int newSize,
          required List<int> oldRoot,
          required List<int> newRoot,
          required List<int> proof}) =>
      _call((s) => _flag(
          s,
          (out) => _b.verifyConsistency(
              oldSize,
              newSize,
              s.bytes(oldRoot),
              oldRoot.length,
              s.bytes(newRoot),
              newRoot.length,
              s.bytes(proof),
              proof.length,
              out)));

  /// Encrypts a file for upload (the server stores ciphertext only).
  EncryptedFile encryptFile(List<int> plaintext,
          {required String mediaType, required String fileName}) =>
      _call((s) {
        final mt = utf8.encode(mediaType);
        final fn = utf8.encode(fileName);
        final out = s.track(calloc<Pointer<CfmEncryptedFile>>());
        _check(_b.encryptFile(s.bytes(plaintext), plaintext.length, s.bytes(mt),
            mt.length, s.bytes(fn), fn.length, out));
        return EncryptedFile._(this, out.value);
      });

  /// Decrypts a downloaded attachment (refuses tampering and a foreign descriptor).
  DecryptedFile decryptFile(
          {required List<int> descriptor,
          required String attachmentId,
          required List<int> ciphertext}) =>
      _call((s) {
        final id = utf8.encode(attachmentId);
        final p = s.buffer(), mt = s.buffer(), fn = s.buffer();
        _check(_b.decryptFile(
            s.bytes(descriptor),
            descriptor.length,
            s.bytes(id),
            id.length,
            s.bytes(ciphertext),
            ciphertext.length,
            p,
            mt,
            fn));
        return DecryptedFile(
            _take(p.ref), _takeString(mt.ref), _takeString(fn.ref));
      });

  /// The protected presentation metadata of a descriptor.
  DescriptorInfo descriptorInfo(List<int> descriptor) => _call((s) {
        final mt = s.buffer(), fn = s.buffer();
        final size = s.track(calloc<Uint64>());
        _check(_b.descriptorInfo(
            s.bytes(descriptor), descriptor.length, mt, fn, size));
        return DescriptorInfo(
            _takeString(mt.ref), _takeString(fn.ref), size.value);
      });

  /// Whether [bytes] is an attachment descriptor.
  bool isDescriptor(List<int> bytes) => _call((s) =>
      _flag(s, (out) => _b.isDescriptor(s.bytes(bytes), bytes.length, out)));

  /// SOURCE: checks the target device signed the transfer request.
  bool verifyTransferRequest({
    required List<int> targetDeviceKey,
    required TransferContext context,
    required List<int> transferKey,
    required int expiresAt,
    required List<int> signature,
  }) =>
      _call((s) => _flag(
          s,
          (out) => _b.verifyTransferRequest(
              s.bytes(targetDeviceKey),
              targetDeviceKey.length,
              s.strings(context.ids),
              7,
              s.bytes(transferKey),
              transferKey.length,
              expiresAt,
              s.bytes(signature),
              signature.length,
              out)));

  /// The short code both devices show during a history transfer.
  String comparisonCode({
    required TransferContext context,
    required List<int> transferKey,
    required List<int> sourceKey,
    required List<int> targetKey,
  }) =>
      _call((s) => utf8.decode(_bytesOut(
          s,
          (out) => _b.comparisonCode(
              s.strings(context.ids),
              7,
              s.bytes(transferKey),
              transferKey.length,
              s.bytes(sourceKey),
              sourceKey.length,
              s.bytes(targetKey),
              targetKey.length,
              out))));

  /// Generates the keys of a new device (server-assigned [deviceId]).
  MessagingDevice createDevice(String deviceId) => _call((s) {
        final id = utf8.encode(deviceId);
        final out = s.track(calloc<Pointer<CfmDevice>>());
        _check(_b.deviceNew(s.bytes(id), id.length, out));
        return MessagingDevice._(this, out.value);
      });

  /// Restores a device from [MessagingDevice.exportState] (decrypted by the app).
  MessagingDevice restoreDevice(String deviceId, List<int> state) => _call((s) {
        final id = utf8.encode(deviceId);
        final out = s.track(calloc<Pointer<CfmDevice>>());
        _check(_b.deviceFromState(
            s.bytes(id), id.length, s.bytes(state), state.length, out));
        return MessagingDevice._(this, out.value);
      });
}

/// A file encrypted for upload. Its descriptor (content key, name, media type) stays in
/// native memory until bound to the server-assigned attachment id.
class EncryptedFile implements Finalizable {
  EncryptedFile._(this._c, this._h) {
    _c._fileFinalizer.attach(this, _h.cast(), detach: this);
  }

  final MessagingCrypto _c;
  Pointer<CfmEncryptedFile> _h;

  Pointer<CfmEncryptedFile> get _handle {
    if (_h == nullptr) throw StateError('EncryptedFile disposed');
    return _h;
  }

  Uint8List get ciphertext => _c._call(
      (s) => _c._bytesOut(s, (o) => _c._b.encryptedFileCiphertext(_handle, o)));

  /// Lower-case hex SHA-256 of the ciphertext (declared in the upload intent).
  String get sha256 => _c._call((s) => utf8
      .decode(_c._bytesOut(s, (o) => _c._b.encryptedFileSha256(_handle, o))));

  int get size => _c._call((s) {
        final out = s.track(calloc<Uint64>());
        _c._check(_c._b.encryptedFileSize(_handle, out));
        return out.value;
      });

  /// The descriptor bound to [attachmentId]. SECRET (carries the content key): place it
  /// only inside an MLS application message.
  Uint8List descriptorFor(String attachmentId) => _c._call((s) {
        final id = utf8.encode(attachmentId);
        return _c._bytesOut(
            s,
            (o) => _c._b.encryptedFileDescriptorFor(
                _handle, s.bytes(id), id.length, o));
      });

  /// Frees the native object (and its content key) now.
  void dispose() {
    if (_h == nullptr) return;
    _c._fileFinalizer.detach(this);
    _c._b.encryptedFileFreePtr
        .asFunction<void Function(Pointer<CfmEncryptedFile>)>()(_h);
    _h = nullptr;
  }
}

/// One device: signature key, KeyPackage private keys, groups and a pending history
/// transfer key, all in native memory. Not safe for concurrent use from several isolates.
class MessagingDevice implements Finalizable {
  MessagingDevice._(this._c, this._h) {
    _c._deviceFinalizer.attach(this, _h.cast(), detach: this);
  }

  final MessagingCrypto _c;
  Pointer<CfmDevice> _h;

  Pointer<CfmDevice> get _handle {
    if (_h == nullptr) throw StateError('MessagingDevice disposed');
    return _h;
  }

  Bindings get _b => _c._b;

  T _call<T>(T Function(_Scope s) body) => _c._call(body);
  Uint8List _out(_Scope s, int Function(Pointer<CfmBuffer>) f) =>
      _c._bytesOut(s, f);

  String get deviceId =>
      _call((s) => utf8.decode(_out(s, (o) => _b.deviceId(_handle, o))));

  Uint8List get signaturePublicKey =>
      _call((s) => _out(s, (o) => _b.deviceSignaturePublicKey(_handle, o)));

  /// One KeyPackage for publication.
  Uint8List keyPackage() =>
      _call((s) => _out(s, (o) => _b.deviceKeyPackage(_handle, o)));

  /// The registration KeyPackage answering a server challenge (32-byte [nonce]).
  Uint8List registrationKeyPackage(String challengeId, List<int> nonce) =>
      _call((s) {
        final id = utf8.encode(challengeId);
        return _out(
            s,
            (o) => _b.deviceRegistrationKeyPackage(_handle, s.bytes(id),
                id.length, s.bytes(nonce), nonce.length, o));
      });

  /// The complete SECRET state (keys and every group). Encrypt it under a
  /// platform-protected key before it leaves memory; overwrite the returned list
  /// (`fillRange(0, length, 0)`) once stored.
  Uint8List exportState() =>
      _call((s) => _out(s, (o) => _b.deviceExportState(_handle, o)));

  bool hasGroup(List<int> groupId) => _call((s) => _c._flag(s,
      (o) => _b.deviceHasGroup(_handle, s.bytes(groupId), groupId.length, o)));

  int _epochCall(_Scope s, int Function(Pointer<Uint64>) f) {
    final out = s.track(calloc<Uint64>());
    _c._check(f(out));
    return out.value;
  }

  /// Creates a group (this device the only member); returns its epoch.
  int createGroup(List<int> groupId) => _call((s) => _epochCall(
      s,
      (o) =>
          _b.deviceCreateGroup(_handle, s.bytes(groupId), groupId.length, o)));

  /// Joins from a Welcome (refused when it is for another group); returns the epoch.
  int join(List<int> groupId, List<int> welcome) => _call((s) => _epochCall(
      s,
      (o) => _b.deviceJoin(_handle, s.bytes(groupId), groupId.length,
          s.bytes(welcome), welcome.length, o)));

  CommitResult _commit(_Scope s, int Function(Pointer<CfmCommitResult>) f) {
    final out = s.track(calloc<CfmCommitResult>());
    _c._check(f(out));
    final r = out.ref;
    final has = r.hasWelcome != 0;
    final commit = _c._take(r.commit);
    final welcome = _c._take(r.welcome);
    return CommitResult(commit, has ? welcome : null, r.epoch);
  }

  /// Commits adding the devices of [keyPackages] and merges it locally.
  CommitResult addDevices(List<int> groupId, List<List<int>> keyPackages) =>
      _call((s) {
        final packed = packByteStrings(keyPackages);
        return _commit(
            s,
            (o) => _b.deviceAddDevices(_handle, s.bytes(groupId),
                groupId.length, s.bytes(packed), packed.length, o));
      });

  /// Commits removing every leaf of [deviceIds] and merges it locally.
  CommitResult removeDevices(List<int> groupId, List<String> deviceIds) =>
      _call((s) => _commit(
          s,
          (o) => _b.deviceRemoveDevices(_handle, s.bytes(groupId),
              groupId.length, s.strings(deviceIds), deviceIds.length, o)));

  /// Encrypts an application message bound to [aad] ([MessagingCrypto.messageAad]).
  Uint8List encrypt(List<int> groupId, List<int> aad, List<int> plaintext) =>
      _call((s) => _out(
          s,
          (o) => _b.deviceEncrypt(
              _handle,
              s.bytes(groupId),
              groupId.length,
              s.bytes(aad),
              aad.length,
              s.bytes(plaintext),
              plaintext.length,
              o)));

  /// Decrypts an application message (only when MLS authenticated exactly
  /// [expectedAad]) or verifies and merges a commit. Tampered, replayed or mis-bound
  /// messages throw [MessagingCryptoException].
  ProcessResult process(List<int> groupId, List<int> message,
          {List<int>? expectedAad}) =>
      _call((s) {
        final out = s.track(calloc<CfmProcessResult>());
        final aad = expectedAad ?? const <int>[];
        _c._check(_b.deviceProcess(
            _handle,
            s.bytes(groupId),
            groupId.length,
            s.bytes(message),
            message.length,
            s.bytes(aad),
            aad.length,
            expectedAad == null ? 0 : 1,
            out));
        final r = out.ref;
        final kind = switch (r.kind) {
          1 => ProcessedKind.application,
          2 => ProcessedKind.commit,
          _ => ProcessedKind.proposal,
        };
        return ProcessResult(kind, _c._take(r.plaintext), r.epoch);
      });

  int epoch(List<int> groupId) => _call((s) => _epochCall(
      s, (o) => _b.deviceEpoch(_handle, s.bytes(groupId), groupId.length, o)));

  /// The group's member device ids, as the MLS tree records them.
  List<String> members(List<int> groupId) => _call((s) {
        final packed = _out(
            s,
            (o) =>
                _b.deviceMembers(_handle, s.bytes(groupId), groupId.length, o));
        return unpackByteStrings(packed).map(utf8.decode).toList();
      });

  /// TARGET: makes the single-use transfer key (kept natively); returns its public half.
  Uint8List beginTransfer() =>
      _call((s) => _out(s, (o) => _b.deviceBeginTransfer(_handle, o)));

  /// TARGET: signs the transfer request with the device key.
  Uint8List signTransferRequest(TransferContext context, int expiresAt) =>
      _call((s) => _out(
          s,
          (o) => _b.deviceSignTransferRequest(
              _handle, s.strings(context.ids), 7, expiresAt, o)));

  /// SOURCE: seals [history] to the target's transfer key and signs it.
  Uint8List sealHistory(
          TransferContext context, List<int> transferKey, List<int> history) =>
      _call((s) => _out(
          s,
          (o) => _b.deviceSealHistory(
              _handle,
              s.strings(context.ids),
              7,
              s.bytes(transferKey),
              transferKey.length,
              s.bytes(history),
              history.length,
              o)));

  /// TARGET: verifies the source signature and opens the archive (the transfer key is
  /// consumed either way).
  Uint8List openHistory(
          List<int> sourceKey, TransferContext context, List<int> sealed) =>
      _call((s) => _out(
          s,
          (o) => _b.deviceOpenHistory(
              _handle,
              s.bytes(sourceKey),
              sourceKey.length,
              s.strings(context.ids),
              7,
              s.bytes(sealed),
              sealed.length,
              o)));

  /// Frees the device and every key it holds now (otherwise when garbage collected).
  void dispose() {
    if (_h == nullptr) return;
    _c._deviceFinalizer.detach(this);
    _b.deviceFreePtr.asFunction<void Function(Pointer<CfmDevice>)>()(_h);
    _h = nullptr;
  }
}
