@Timeout(Duration(minutes: 15))
library;

import 'dart:convert';
import 'dart:typed_data';

import 'package:cfmeet_platform_sdk/cfmeet_platform_sdk.dart';
import 'package:crypto/crypto.dart' as hash;
import 'package:test/test.dart';

import 'support/native_library.dart';

const tenant = '6f1c1f43-2a8e-4e0a-9d43-0d6e5f0b8f01';
const app = '6f1c1f43-2a8e-4e0a-9d43-0d6e5f0b8f02';
const env = '6f1c1f43-2a8e-4e0a-9d43-0d6e5f0b8f03';
const conversation = '6f1c1f43-2a8e-4e0a-9d43-0d6e5f0b8f04';
const aliceId = '0b6a8f9e-1c2d-4e3f-8a9b-0c1d2e3f4a01';
const bobId = '0b6a8f9e-1c2d-4e3f-8a9b-0c1d2e3f4a02';
final Uint8List groupId =
    Uint8List.fromList(utf8.encode('cfmeet-dart-test-group-00000001!'));

Matcher refusedWith(String code) => throwsA(
    isA<MessagingCryptoException>().having((e) => e.code, 'code', code));

void main() {
  late MessagingCrypto mc;

  setUpAll(() => mc = MessagingCrypto.open(path: nativeLibraryPath()));

  Uint8List aad(String sender,
          {String message = '6f1c1f43-2a8e-4e0a-9d43-0d6e5f0b8f05'}) =>
      mc.messageAad(
          tenantId: tenant,
          applicationId: app,
          environmentId: env,
          conversationId: conversation,
          clientMessageId: message,
          senderDeviceId: sender);

  (MessagingDevice, MessagingDevice) group2() {
    final alice = mc.createDevice(aliceId);
    final bob = mc.createDevice(bobId);
    expect(alice.createGroup(groupId), 0);
    final commit = alice.addDevices(groupId, [bob.keyPackage()]);
    expect(commit.epoch, 0);
    expect(commit.welcome, isNotNull);
    expect(bob.join(groupId, commit.welcome!), 1);
    return (alice, bob);
  }

  test('loads the real library at the expected ABI version', () {
    expect(mc.abiVersion, messagingCryptoAbiVersion);
    expect(() => MessagingCrypto.open(path: '/nonexistent/libnope.so'),
        throwsA(isA<MessagingCryptoLoadError>()));
  });

  test('device keys -> KeyPackage -> validated inspection', () {
    final d = mc.createDevice(aliceId);
    expect(d.deviceId, aliceId);
    expect(d.signaturePublicKey, hasLength(32));
    final kp = d.keyPackage();
    final info = mc.inspectKeyPackage(kp);
    expect(info.deviceId, aliceId);
    expect(info.signatureKey, d.signaturePublicKey);

    final tampered = Uint8List.fromList(kp)..[kp.length - 10] ^= 1;
    expect(() => mc.inspectKeyPackage(tampered),
        throwsA(isA<MessagingCryptoException>()));
    // Lifetime: a KeyPackage is not valid far in the future.
    expect(() => mc.inspectKeyPackage(kp, now: DateTime.utc(2100)),
        throwsA(isA<MessagingCryptoException>()));
    final reg = d.registrationKeyPackage(
        '8d0b1a5e-7f7c-4d4e-9a3b-2b2c2d2e2f30', List.filled(32, 7));
    expect(mc.inspectKeyPackage(reg).deviceId, aliceId);
    expect(
        () => d.registrationKeyPackage(
            '8d0b1a5e-7f7c-4d4e-9a3b-2b2c2d2e2f30', [1, 2]),
        refusedWith('BAD_NONCE'));
    d.dispose();
  });

  test('argument checks fail closed with stable codes', () {
    expect(() => mc.createDevice('not-a-uuid'), refusedWith('BAD_UUID'));
    expect(
        () => mc.messageAad(
            tenantId: tenant,
            applicationId: app,
            environmentId: env,
            conversationId: 'x',
            clientMessageId: tenant,
            senderDeviceId: tenant),
        refusedWith('BAD_UUID'));
  });

  test(
      'two devices: group, encrypt / decrypt, replay / tamper / AAD refusal, removal',
      () {
    final (alice, bob) = group2();
    final a = aad(aliceId);
    final ct = alice.encrypt(groupId, a, utf8.encode('hello bob'));
    final r = bob.process(groupId, ct, expectedAad: a);
    expect(r.kind, ProcessedKind.application);
    expect(utf8.decode(r.plaintext), 'hello bob');
    expect(r.epoch, 1);

    expect(() => bob.process(groupId, ct, expectedAad: a),
        refusedWith('MESSAGE_REFUSED'));
    final second = alice.encrypt(groupId, a, utf8.encode('second'));
    final tampered = Uint8List.fromList(second)..[second.length - 5] ^= 0x80;
    expect(() => bob.process(groupId, tampered, expectedAad: a),
        refusedWith('MESSAGE_REFUSED'));
    final third = alice.encrypt(groupId, a, utf8.encode('third'));
    expect(() => bob.process(groupId, third, expectedAad: aad(bobId)),
        refusedWith('AAD_MISMATCH'));
    final fourth = alice.encrypt(groupId, a, utf8.encode('fourth'));
    expect(() => bob.process(groupId, fourth), refusedWith('AAD_MISMATCH'));

    expect(bob.members(groupId)..sort(), [aliceId, bobId]);
    expect(bob.hasGroup(groupId), isTrue);
    expect(bob.hasGroup(Uint8List(32)), isFalse);
    expect(() => alice.createGroup(groupId), refusedWith('GROUP_EXISTS'));
    expect(() => alice.epoch(Uint8List(32)), refusedWith('UNKNOWN_GROUP'));
    expect(() => alice.addDevices(groupId, []), refusedWith('NO_KEY_PACKAGES'));
    expect(() => alice.addDevices(groupId, [utf8.encode('garbage')]),
        refusedWith('ADD_REFUSED'));

    final removal = alice.removeDevices(groupId, [bobId]);
    expect(removal.welcome, isNull);
    expect(alice.epoch(groupId), 2);
    expect(bob.process(groupId, removal.commit).kind, ProcessedKind.commit);
    final after = alice.encrypt(groupId, a, utf8.encode('bob is gone'));
    expect(() => bob.process(groupId, after, expectedAad: a),
        throwsA(isA<MessagingCryptoException>()));
    alice.dispose();
    bob.dispose();
  });

  test('a Welcome for another group is refused', () {
    final alice = mc.createDevice(aliceId);
    final bob = mc.createDevice(bobId);
    alice.createGroup(groupId);
    final c = alice.addDevices(groupId, [bob.keyPackage()]);
    expect(() => bob.join(Uint8List(32), c.welcome!),
        refusedWith('WELCOME_GROUP_MISMATCH'));
    expect(() => bob.join(groupId, [1, 2, 3]), refusedWith('WELCOME_REFUSED'));
  });

  test(
      'exported state restores the device and its groups, only under its own id',
      () {
    final (alice, bob) = group2();
    final state = bob.exportState();
    bob.dispose();
    final restored = mc.restoreDevice(bobId, state);
    final a = aad(aliceId);
    final ct = alice.encrypt(groupId, a, utf8.encode('after restore'));
    expect(utf8.decode(restored.process(groupId, ct, expectedAad: a).plaintext),
        'after restore');
    expect(() => mc.restoreDevice(aliceId, state),
        refusedWith('DEVICE_STATE_INVALID'));
    expect(() => mc.restoreDevice(bobId, state.sublist(0, state.length - 1)),
        refusedWith('DEVICE_STATE_INVALID'));
    state.fillRange(0, state.length, 0);
  });

  test(
      'attachments: encrypt, bind, decrypt, refuse tampering and foreign descriptors',
      () {
    final pt = utf8.encode('%PDF-1.7 attachment body');
    final f =
        mc.encryptFile(pt, mediaType: 'application/pdf', fileName: 'a.pdf');
    final ct = f.ciphertext;
    expect(f.size, ct.length);
    expect(f.sha256, hash.sha256.convert(ct).toString());
    const att = '3c4d5e6f-7a8b-4c9d-8e0f-1a2b3c4d5e6f';
    final desc = f.descriptorFor(att);
    f.dispose();
    f.dispose(); // idempotent
    expect(() => f.ciphertext, throwsStateError);

    expect(mc.isDescriptor(desc), isTrue);
    expect(mc.isDescriptor(utf8.encode('hello')), isFalse);
    final info = mc.descriptorInfo(desc);
    expect((info.mediaType, info.fileName, info.size),
        ('application/pdf', 'a.pdf', pt.length));
    final out =
        mc.decryptFile(descriptor: desc, attachmentId: att, ciphertext: ct);
    expect(out.plaintext, pt);
    expect(out.fileName, 'a.pdf');
    final bad = Uint8List.fromList(ct)..[20] ^= 1;
    expect(
        () => mc.decryptFile(
            descriptor: desc, attachmentId: att, ciphertext: bad),
        refusedWith('ATTACHMENT_TAMPERED'));
    expect(
        () => mc.decryptFile(
            descriptor: desc,
            attachmentId: '3c4d5e6f-7a8b-4c9d-8e0f-1a2b3c4d5e70',
            ciphertext: ct),
        refusedWith('DESCRIPTOR_ATTACHMENT_MISMATCH'));
    expect(() => mc.descriptorInfo([1, 2, 3]), refusedWith('BAD_DESCRIPTOR'));
  });

  test('key transparency proofs', () {
    // RFC 9162 hashes, computed here only to build test inputs.
    List<int> leaf(String d) =>
        hash.sha256.convert([0, ...utf8.encode(d)]).bytes;
    List<int> node(List<int> l, List<int> r) =>
        hash.sha256.convert([1, ...l, ...r]).bytes;
    final a = leaf('a'), b = leaf('b');
    final root = node(a, b);
    expect(
        mc.verifyInclusion(
            index: 0,
            size: 2,
            leafData: utf8.encode('a'),
            proof: b,
            root: root),
        isTrue);
    expect(
        mc.verifyInclusion(
            index: 1,
            size: 2,
            leafData: utf8.encode('a'),
            proof: b,
            root: root),
        isFalse);
    expect(
        () => mc.verifyInclusion(
            index: 0,
            size: 2,
            leafData: utf8.encode('a'),
            proof: b.sublist(1),
            root: root),
        refusedWith('KT_BAD_PROOF'));
    expect(
        mc.verifyConsistency(
            oldSize: 1, newSize: 2, oldRoot: a, newRoot: root, proof: b),
        isTrue);
    expect(
        mc.verifyConsistency(
            oldSize: 1, newSize: 2, oldRoot: b, newRoot: root, proof: b),
        isFalse);
    expect(
        mc.verifyTreeHead(
            logPublicKey: List.filled(32, 1),
            logId: 'log',
            treeSize: 2,
            rootHash: root,
            timestampMicros: 0,
            signature: List.filled(64, 0)),
        isFalse);
  });

  test('history transfer between two devices', () {
    final source = mc.createDevice(aliceId);
    final target = mc.createDevice(bobId);
    const ctx = TransferContext(
      transferId: 'aaaaaaaa-0000-4000-8000-000000000001',
      tenantId: tenant,
      applicationId: app,
      environmentId: env,
      principalId: 'aaaaaaaa-0000-4000-8000-000000000005',
      sourceDeviceId: aliceId,
      targetDeviceId: bobId,
    );
    expect(() => target.signTransferRequest(ctx, 4000000000),
        refusedWith('NO_TRANSFER'));
    final tkey = target.beginTransfer();
    final sig = target.signTransferRequest(ctx, 4000000000);
    expect(
        mc.verifyTransferRequest(
            targetDeviceKey: target.signaturePublicKey,
            context: ctx,
            transferKey: tkey,
            expiresAt: 4000000000,
            signature: sig),
        isTrue);
    expect(
        mc.verifyTransferRequest(
            targetDeviceKey: source.signaturePublicKey,
            context: ctx,
            transferKey: tkey,
            expiresAt: 4000000000,
            signature: sig),
        isFalse);
    final code = mc.comparisonCode(
        context: ctx,
        transferKey: tkey,
        sourceKey: source.signaturePublicKey,
        targetKey: target.signaturePublicKey);
    expect(code, isNotEmpty);
    final sealed = source.sealHistory(ctx, tkey, utf8.encode('archive'));
    expect(
        utf8.decode(target.openHistory(source.signaturePublicKey, ctx, sealed)),
        'archive');
    // Single use.
    expect(() => target.openHistory(source.signaturePublicKey, ctx, sealed),
        refusedWith('NO_TRANSFER'));
  });

  test('disposed handles are refused; packing helpers round-trip', () {
    final d = mc.createDevice(aliceId)..dispose();
    expect(() => d.keyPackage(), throwsStateError);
    d.dispose();
    final packed = packByteStrings([
      [1, 2],
      [],
      [3]
    ]);
    expect(unpackByteStrings(packed), [
      [1, 2],
      <int>[],
      [3]
    ]);
    expect(() => unpackByteStrings(Uint8List.fromList([0, 0, 0, 5, 1])),
        throwsFormatException);
  });
}
