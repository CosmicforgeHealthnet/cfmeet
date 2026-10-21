export class CommitResult {
    static __wrap(ptr) {
        const obj = Object.create(CommitResult.prototype);
        obj.__wbg_ptr = ptr;
        CommitResultFinalization.register(obj, obj.__wbg_ptr, obj);
        return obj;
    }
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        CommitResultFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_commitresult_free(ptr, 0);
    }
    /**
     * @returns {Uint8Array}
     */
    get commit() {
        const ret = wasm.commitresult_commit(this.__wbg_ptr);
        var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v1;
    }
    /**
     * The epoch the commit was created in (what the Delivery Service orders on).
     * @returns {bigint}
     */
    get epoch() {
        const ret = wasm.commitresult_epoch(this.__wbg_ptr);
        return BigInt.asUintN(64, ret);
    }
    /**
     * @returns {Uint8Array | undefined}
     */
    get welcome() {
        const ret = wasm.commitresult_welcome(this.__wbg_ptr);
        let v1;
        if (ret[0] !== 0) {
            v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
            wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        }
        return v1;
    }
}
if (Symbol.dispose) CommitResult.prototype[Symbol.dispose] = CommitResult.prototype.free;

export class DecryptedFile {
    static __wrap(ptr) {
        const obj = Object.create(DecryptedFile.prototype);
        obj.__wbg_ptr = ptr;
        DecryptedFileFinalization.register(obj, obj.__wbg_ptr, obj);
        return obj;
    }
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        DecryptedFileFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_decryptedfile_free(ptr, 0);
    }
    /**
     * @returns {string}
     */
    get fileName() {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.decryptedfile_fileName(this.__wbg_ptr);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * @returns {string}
     */
    get mediaType() {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.decryptedfile_mediaType(this.__wbg_ptr);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * @returns {Uint8Array}
     */
    get plaintext() {
        const ret = wasm.decryptedfile_plaintext(this.__wbg_ptr);
        var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v1;
    }
}
if (Symbol.dispose) DecryptedFile.prototype[Symbol.dispose] = DecryptedFile.prototype.free;

/**
 * The protected presentation metadata of a descriptor (shown before download).
 */
export class DescriptorInfo {
    static __wrap(ptr) {
        const obj = Object.create(DescriptorInfo.prototype);
        obj.__wbg_ptr = ptr;
        DescriptorInfoFinalization.register(obj, obj.__wbg_ptr, obj);
        return obj;
    }
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        DescriptorInfoFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_descriptorinfo_free(ptr, 0);
    }
    /**
     * @returns {string}
     */
    get fileName() {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.descriptorinfo_fileName(this.__wbg_ptr);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * @returns {string}
     */
    get mediaType() {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.descriptorinfo_mediaType(this.__wbg_ptr);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * @returns {bigint}
     */
    get size() {
        const ret = wasm.descriptorinfo_size(this.__wbg_ptr);
        return BigInt.asUintN(64, ret);
    }
}
if (Symbol.dispose) DescriptorInfo.prototype[Symbol.dispose] = DescriptorInfo.prototype.free;

/**
 * A file encrypted for upload. Its descriptor (content key, file name, media type) stays
 * inside this object until it is bound to the attachment id the server assigned
 * ([`EncryptedFile::descriptor_for`]) and travels inside an MLS message.
 */
export class EncryptedFile {
    static __wrap(ptr) {
        const obj = Object.create(EncryptedFile.prototype);
        obj.__wbg_ptr = ptr;
        EncryptedFileFinalization.register(obj, obj.__wbg_ptr, obj);
        return obj;
    }
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        EncryptedFileFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_encryptedfile_free(ptr, 0);
    }
    /**
     * @returns {Uint8Array}
     */
    get ciphertext() {
        const ret = wasm.encryptedfile_ciphertext(this.__wbg_ptr);
        var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v1;
    }
    /**
     * The descriptor bound to the server-assigned attachment id (SECRET: it carries the
     * content key; it is only ever placed inside an MLS application message).
     * @param {string} attachment_id
     * @returns {Uint8Array}
     */
    descriptorFor(attachment_id) {
        const ptr0 = passStringToWasm0(attachment_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.encryptedfile_descriptorFor(this.__wbg_ptr, ptr0, len0);
        if (ret[3]) {
            throw takeFromExternrefTable0(ret[2]);
        }
        var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v2;
    }
    /**
     * Lower-case hex SHA-256 of the ciphertext (what the upload intent declares).
     * @returns {string}
     */
    get sha256() {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.encryptedfile_sha256(this.__wbg_ptr);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * @returns {bigint}
     */
    get size() {
        const ret = wasm.encryptedfile_size(this.__wbg_ptr);
        return BigInt.asUintN(64, ret);
    }
}
if (Symbol.dispose) EncryptedFile.prototype[Symbol.dispose] = EncryptedFile.prototype.free;

/**
 * What a published KeyPackage says about its device (after full validation: signature,
 * cipher suite, lifetime at `now` Unix seconds).
 */
export class KeyPackageInfo {
    static __wrap(ptr) {
        const obj = Object.create(KeyPackageInfo.prototype);
        obj.__wbg_ptr = ptr;
        KeyPackageInfoFinalization.register(obj, obj.__wbg_ptr, obj);
        return obj;
    }
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        KeyPackageInfoFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_keypackageinfo_free(ptr, 0);
    }
    /**
     * @returns {string}
     */
    get deviceId() {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.keypackageinfo_deviceId(this.__wbg_ptr);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * @returns {Uint8Array}
     */
    get signatureKey() {
        const ret = wasm.keypackageinfo_signatureKey(this.__wbg_ptr);
        var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v1;
    }
}
if (Symbol.dispose) KeyPackageInfo.prototype[Symbol.dispose] = KeyPackageInfo.prototype.free;

export class ProcessResult {
    static __wrap(ptr) {
        const obj = Object.create(ProcessResult.prototype);
        obj.__wbg_ptr = ptr;
        ProcessResultFinalization.register(obj, obj.__wbg_ptr, obj);
        return obj;
    }
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        ProcessResultFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_processresult_free(ptr, 0);
    }
    /**
     * @returns {bigint}
     */
    get epoch() {
        const ret = wasm.processresult_epoch(this.__wbg_ptr);
        return BigInt.asUintN(64, ret);
    }
    /**
     * `APPLICATION`, `COMMIT` or `PROPOSAL`.
     * @returns {string}
     */
    get kind() {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.processresult_kind(this.__wbg_ptr);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * @returns {Uint8Array}
     */
    get plaintext() {
        const ret = wasm.processresult_plaintext(this.__wbg_ptr);
        var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v1;
    }
}
if (Symbol.dispose) ProcessResult.prototype[Symbol.dispose] = ProcessResult.prototype.free;

/**
 * One device: its MLS signature key, KeyPackage private keys, groups and (while a
 * history transfer is pending) its single-use transfer key, all inside this worker.
 */
export class WebDevice {
    static __wrap(ptr) {
        const obj = Object.create(WebDevice.prototype);
        obj.__wbg_ptr = ptr;
        WebDeviceFinalization.register(obj, obj.__wbg_ptr, obj);
        return obj;
    }
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        WebDeviceFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_webdevice_free(ptr, 0);
    }
    /**
     * Commits adding the devices of the packed KeyPackages (`[u32 len][bytes]...`, each
     * validated by OpenMLS: signature, suite, lifetime) and merges it locally.
     * @param {Uint8Array} group_id
     * @param {Uint8Array} packed
     * @returns {CommitResult}
     */
    addDevices(group_id, packed) {
        const ptr0 = passArray8ToWasm0(group_id, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArray8ToWasm0(packed, wasm.__wbindgen_malloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.webdevice_addDevices(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return CommitResult.__wrap(ret[0]);
    }
    /**
     * TARGET of a history transfer: makes the single-use transfer key (kept here) and
     * returns its public half.
     * @returns {Uint8Array}
     */
    beginTransfer() {
        const ret = wasm.webdevice_beginTransfer(this.__wbg_ptr);
        if (ret[3]) {
            throw takeFromExternrefTable0(ret[2]);
        }
        var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v1;
    }
    /**
     * @param {Uint8Array} group_id
     * @returns {bigint}
     */
    createGroup(group_id) {
        const ptr0 = passArray8ToWasm0(group_id, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.webdevice_createGroup(this.__wbg_ptr, ptr0, len0);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return BigInt.asUintN(64, ret[0]);
    }
    /**
     * @returns {string}
     */
    get deviceId() {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.webdevice_deviceId(this.__wbg_ptr);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * @param {Uint8Array} group_id
     * @param {Uint8Array} aad
     * @param {Uint8Array} plaintext
     * @returns {Uint8Array}
     */
    encrypt(group_id, aad, plaintext) {
        const ptr0 = passArray8ToWasm0(group_id, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArray8ToWasm0(aad, wasm.__wbindgen_malloc);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passArray8ToWasm0(plaintext, wasm.__wbindgen_malloc);
        const len2 = WASM_VECTOR_LEN;
        const ret = wasm.webdevice_encrypt(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2);
        if (ret[3]) {
            throw takeFromExternrefTable0(ret[2]);
        }
        var v4 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v4;
    }
    /**
     * @param {Uint8Array} group_id
     * @returns {bigint}
     */
    epoch(group_id) {
        const ptr0 = passArray8ToWasm0(group_id, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.webdevice_epoch(this.__wbg_ptr, ptr0, len0);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return BigInt.asUintN(64, ret[0]);
    }
    /**
     * The complete secret state (keys and every group). The worker encrypts it before
     * it leaves memory and never posts it out of the worker.
     * @returns {Uint8Array}
     */
    exportState() {
        const ret = wasm.webdevice_exportState(this.__wbg_ptr);
        var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v1;
    }
    /**
     * Restores a device from [`WebDevice::export_state`] (decrypted by the worker).
     * @param {string} device_id
     * @param {Uint8Array} state
     * @returns {WebDevice}
     */
    static fromState(device_id, state) {
        const ptr0 = passStringToWasm0(device_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArray8ToWasm0(state, wasm.__wbindgen_malloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.webdevice_fromState(ptr0, len0, ptr1, len1);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return WebDevice.__wrap(ret[0]);
    }
    /**
     * @param {Uint8Array} group_id
     * @returns {boolean}
     */
    hasGroup(group_id) {
        const ptr0 = passArray8ToWasm0(group_id, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.webdevice_hasGroup(this.__wbg_ptr, ptr0, len0);
        return ret !== 0;
    }
    /**
     * Joins from a Welcome; refuses a Welcome of another group than `group_id`.
     * @param {Uint8Array} group_id
     * @param {Uint8Array} welcome
     * @returns {bigint}
     */
    join(group_id, welcome) {
        const ptr0 = passArray8ToWasm0(group_id, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArray8ToWasm0(welcome, wasm.__wbindgen_malloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.webdevice_join(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return BigInt.asUintN(64, ret[0]);
    }
    /**
     * @returns {Uint8Array}
     */
    keyPackage() {
        const ret = wasm.webdevice_keyPackage(this.__wbg_ptr);
        if (ret[3]) {
            throw takeFromExternrefTable0(ret[2]);
        }
        var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v1;
    }
    /**
     * The group's member device ids, as the MLS tree records them.
     * @param {Uint8Array} group_id
     * @returns {string[]}
     */
    members(group_id) {
        const ptr0 = passArray8ToWasm0(group_id, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.webdevice_members(this.__wbg_ptr, ptr0, len0);
        if (ret[3]) {
            throw takeFromExternrefTable0(ret[2]);
        }
        var v2 = getArrayJsValueFromWasm0(ret[0], ret[1]);
        wasm.__wbindgen_free(ret[0], ret[1] * 4, 4);
        return v2;
    }
    /**
     * Generates the device keys in the browser (Web Crypto randomness).
     * @param {string} device_id
     */
    constructor(device_id) {
        const ptr0 = passStringToWasm0(device_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.webdevice_new(ptr0, len0);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        this.__wbg_ptr = ret[0];
        WebDeviceFinalization.register(this, this.__wbg_ptr, this);
        return this;
    }
    /**
     * TARGET: verifies the source device signature and opens the archive. The transfer
     * key is consumed (single use) whether or not opening succeeds.
     * @param {Uint8Array} source_key
     * @param {string[]} ctx
     * @param {Uint8Array} sealed
     * @returns {Uint8Array}
     */
    openHistory(source_key, ctx, sealed) {
        const ptr0 = passArray8ToWasm0(source_key, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArrayJsValueToWasm0(ctx, wasm.__wbindgen_malloc);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passArray8ToWasm0(sealed, wasm.__wbindgen_malloc);
        const len2 = WASM_VECTOR_LEN;
        const ret = wasm.webdevice_openHistory(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2);
        if (ret[3]) {
            throw takeFromExternrefTable0(ret[2]);
        }
        var v4 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v4;
    }
    /**
     * Decrypts an application message (only when MLS authenticated exactly
     * `expected_aad` with it: the CF Meet binding of tenant, application, environment,
     * conversation, client message id and sending device) or verifies and merges a
     * commit. A mismatched AAD, a tampered or replayed message is refused.
     * @param {Uint8Array} group_id
     * @param {Uint8Array} message
     * @param {Uint8Array | null} [expected_aad]
     * @returns {ProcessResult}
     */
    process(group_id, message, expected_aad) {
        const ptr0 = passArray8ToWasm0(group_id, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArray8ToWasm0(message, wasm.__wbindgen_malloc);
        const len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(expected_aad) ? 0 : passArray8ToWasm0(expected_aad, wasm.__wbindgen_malloc);
        var len2 = WASM_VECTOR_LEN;
        const ret = wasm.webdevice_process(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return ProcessResult.__wrap(ret[0]);
    }
    /**
     * The registration KeyPackage answering a server challenge (device binding).
     * @param {string} challenge_id
     * @param {Uint8Array} nonce
     * @returns {Uint8Array}
     */
    registrationKeyPackage(challenge_id, nonce) {
        const ptr0 = passStringToWasm0(challenge_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArray8ToWasm0(nonce, wasm.__wbindgen_malloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.webdevice_registrationKeyPackage(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        if (ret[3]) {
            throw takeFromExternrefTable0(ret[2]);
        }
        var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v3;
    }
    /**
     * Commits removing every leaf of `device_ids` and merges it locally.
     * @param {Uint8Array} group_id
     * @param {string[]} device_ids
     * @returns {CommitResult}
     */
    removeDevices(group_id, device_ids) {
        const ptr0 = passArray8ToWasm0(group_id, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArrayJsValueToWasm0(device_ids, wasm.__wbindgen_malloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.webdevice_removeDevices(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return CommitResult.__wrap(ret[0]);
    }
    /**
     * SOURCE: seals the history archive to the target's transfer key and signs it.
     * @param {string[]} ctx
     * @param {Uint8Array} transfer_key
     * @param {Uint8Array} history
     * @returns {Uint8Array}
     */
    sealHistory(ctx, transfer_key, history) {
        const ptr0 = passArrayJsValueToWasm0(ctx, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArray8ToWasm0(transfer_key, wasm.__wbindgen_malloc);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passArray8ToWasm0(history, wasm.__wbindgen_malloc);
        const len2 = WASM_VECTOR_LEN;
        const ret = wasm.webdevice_sealHistory(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2);
        if (ret[3]) {
            throw takeFromExternrefTable0(ret[2]);
        }
        var v4 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v4;
    }
    /**
     * TARGET: signs the transfer request with the device key.
     * @param {string[]} ctx
     * @param {bigint} expires_at
     * @returns {Uint8Array}
     */
    signTransferRequest(ctx, expires_at) {
        const ptr0 = passArrayJsValueToWasm0(ctx, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.webdevice_signTransferRequest(this.__wbg_ptr, ptr0, len0, expires_at);
        if (ret[3]) {
            throw takeFromExternrefTable0(ret[2]);
        }
        var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v2;
    }
    /**
     * @returns {Uint8Array}
     */
    get signaturePublicKey() {
        const ret = wasm.webdevice_signaturePublicKey(this.__wbg_ptr);
        var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v1;
    }
}
if (Symbol.dispose) WebDevice.prototype[Symbol.dispose] = WebDevice.prototype.free;

/**
 * The short code both devices show; the user confirms they match.
 * @param {string[]} ctx
 * @param {Uint8Array} transfer_key
 * @param {Uint8Array} source_key
 * @param {Uint8Array} target_key
 * @returns {string}
 */
export function comparison_code(ctx, transfer_key, source_key, target_key) {
    let deferred6_0;
    let deferred6_1;
    try {
        const ptr0 = passArrayJsValueToWasm0(ctx, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArray8ToWasm0(transfer_key, wasm.__wbindgen_malloc);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passArray8ToWasm0(source_key, wasm.__wbindgen_malloc);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passArray8ToWasm0(target_key, wasm.__wbindgen_malloc);
        const len3 = WASM_VECTOR_LEN;
        const ret = wasm.comparison_code(ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3);
        var ptr5 = ret[0];
        var len5 = ret[1];
        if (ret[3]) {
            ptr5 = 0; len5 = 0;
            throw takeFromExternrefTable0(ret[2]);
        }
        deferred6_0 = ptr5;
        deferred6_1 = len5;
        return getStringFromWasm0(ptr5, len5);
    } finally {
        wasm.__wbindgen_free(deferred6_0, deferred6_1, 1);
    }
}

/**
 * Decrypts a downloaded attachment. Refuses a ciphertext that does not match the
 * descriptor's digest or authentication, and a descriptor of another attachment.
 * @param {Uint8Array} descriptor
 * @param {string} attachment_id
 * @param {Uint8Array} ciphertext
 * @returns {DecryptedFile}
 */
export function decrypt_file(descriptor, attachment_id, ciphertext) {
    const ptr0 = passArray8ToWasm0(descriptor, wasm.__wbindgen_malloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(attachment_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ptr2 = passArray8ToWasm0(ciphertext, wasm.__wbindgen_malloc);
    const len2 = WASM_VECTOR_LEN;
    const ret = wasm.decrypt_file(ptr0, len0, ptr1, len1, ptr2, len2);
    if (ret[2]) {
        throw takeFromExternrefTable0(ret[1]);
    }
    return DecryptedFile.__wrap(ret[0]);
}

/**
 * @param {Uint8Array} descriptor
 * @returns {DescriptorInfo}
 */
export function descriptor_info(descriptor) {
    const ptr0 = passArray8ToWasm0(descriptor, wasm.__wbindgen_malloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.descriptor_info(ptr0, len0);
    if (ret[2]) {
        throw takeFromExternrefTable0(ret[1]);
    }
    return DescriptorInfo.__wrap(ret[0]);
}

/**
 * Encrypts a file for upload (client-side; the server stores ciphertext only).
 * @param {Uint8Array} plaintext
 * @param {string} media_type
 * @param {string} file_name
 * @returns {EncryptedFile}
 */
export function encrypt_file(plaintext, media_type, file_name) {
    const ptr0 = passArray8ToWasm0(plaintext, wasm.__wbindgen_malloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(media_type, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ptr2 = passStringToWasm0(file_name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len2 = WASM_VECTOR_LEN;
    const ret = wasm.encrypt_file(ptr0, len0, ptr1, len1, ptr2, len2);
    if (ret[2]) {
        throw takeFromExternrefTable0(ret[1]);
    }
    return EncryptedFile.__wrap(ret[0]);
}

/**
 * @param {Uint8Array} encoded
 * @param {bigint} now
 * @returns {KeyPackageInfo}
 */
export function inspect_key_package(encoded, now) {
    const ptr0 = passArray8ToWasm0(encoded, wasm.__wbindgen_malloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.inspect_key_package(ptr0, len0, now);
    if (ret[2]) {
        throw takeFromExternrefTable0(ret[1]);
    }
    return KeyPackageInfo.__wrap(ret[0]);
}

/**
 * Whether `bytes` is an attachment descriptor (an encrypted attachment message).
 * @param {Uint8Array} bytes
 * @returns {boolean}
 */
export function is_descriptor(bytes) {
    const ptr0 = passArray8ToWasm0(bytes, wasm.__wbindgen_malloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.is_descriptor(ptr0, len0);
    return ret !== 0;
}

/**
 * The CF Meet AAD of one application message (binds tenant, application, environment,
 * conversation, client message id and sending device).
 * @param {string} tenant
 * @param {string} application
 * @param {string} environment
 * @param {string} conversation
 * @param {string} client_message
 * @param {string} sender_device
 * @returns {Uint8Array}
 */
export function message_aad(tenant, application, environment, conversation, client_message, sender_device) {
    const ptr0 = passStringToWasm0(tenant, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(application, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ptr2 = passStringToWasm0(environment, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len2 = WASM_VECTOR_LEN;
    const ptr3 = passStringToWasm0(conversation, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len3 = WASM_VECTOR_LEN;
    const ptr4 = passStringToWasm0(client_message, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len4 = WASM_VECTOR_LEN;
    const ptr5 = passStringToWasm0(sender_device, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len5 = WASM_VECTOR_LEN;
    const ret = wasm.message_aad(ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, ptr5, len5);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v7 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v7;
}

/**
 * Verifies that the tree of `new_size` / `new_root` extends the one this device saw
 * (no rewritten or rolled-back history).
 * @param {bigint} old_size
 * @param {bigint} new_size
 * @param {Uint8Array} old_root
 * @param {Uint8Array} new_root
 * @param {Uint8Array} proof
 * @returns {boolean}
 */
export function verify_consistency(old_size, new_size, old_root, new_root, proof) {
    const ptr0 = passArray8ToWasm0(old_root, wasm.__wbindgen_malloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passArray8ToWasm0(new_root, wasm.__wbindgen_malloc);
    const len1 = WASM_VECTOR_LEN;
    const ptr2 = passArray8ToWasm0(proof, wasm.__wbindgen_malloc);
    const len2 = WASM_VECTOR_LEN;
    const ret = wasm.verify_consistency(old_size, new_size, ptr0, len0, ptr1, len1, ptr2, len2);
    if (ret[2]) {
        throw takeFromExternrefTable0(ret[1]);
    }
    return ret[0] !== 0;
}

/**
 * Verifies that `leaf_data` is leaf `index` of the tree of `size` with `root`.
 * `proof` is the concatenation of the 32-byte proof hashes.
 * @param {bigint} index
 * @param {bigint} size
 * @param {Uint8Array} leaf_data
 * @param {Uint8Array} proof
 * @param {Uint8Array} root
 * @returns {boolean}
 */
export function verify_inclusion(index, size, leaf_data, proof, root) {
    const ptr0 = passArray8ToWasm0(leaf_data, wasm.__wbindgen_malloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passArray8ToWasm0(proof, wasm.__wbindgen_malloc);
    const len1 = WASM_VECTOR_LEN;
    const ptr2 = passArray8ToWasm0(root, wasm.__wbindgen_malloc);
    const len2 = WASM_VECTOR_LEN;
    const ret = wasm.verify_inclusion(index, size, ptr0, len0, ptr1, len1, ptr2, len2);
    if (ret[2]) {
        throw takeFromExternrefTable0(ret[1]);
    }
    return ret[0] !== 0;
}

/**
 * SOURCE: checks the target device signed the transfer request with its registered key.
 * @param {Uint8Array} target_device_key
 * @param {string[]} ctx
 * @param {Uint8Array} transfer_key
 * @param {bigint} expires_at
 * @param {Uint8Array} signature
 * @returns {boolean}
 */
export function verify_transfer_request(target_device_key, ctx, transfer_key, expires_at, signature) {
    const ptr0 = passArray8ToWasm0(target_device_key, wasm.__wbindgen_malloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passArrayJsValueToWasm0(ctx, wasm.__wbindgen_malloc);
    const len1 = WASM_VECTOR_LEN;
    const ptr2 = passArray8ToWasm0(transfer_key, wasm.__wbindgen_malloc);
    const len2 = WASM_VECTOR_LEN;
    const ptr3 = passArray8ToWasm0(signature, wasm.__wbindgen_malloc);
    const len3 = WASM_VECTOR_LEN;
    const ret = wasm.verify_transfer_request(ptr0, len0, ptr1, len1, ptr2, len2, expires_at, ptr3, len3);
    if (ret[2]) {
        throw takeFromExternrefTable0(ret[1]);
    }
    return ret[0] !== 0;
}

/**
 * Verifies a key-transparency tree head signed by the pinned log key (RFC 9162 heads,
 * Part V G.5). `false`: forged, substituted log or wrong key.
 * @param {Uint8Array} log_public_key
 * @param {string} log_id
 * @param {bigint} tree_size
 * @param {Uint8Array} root_hash
 * @param {bigint} timestamp_micros
 * @param {Uint8Array} signature
 * @returns {boolean}
 */
export function verify_tree_head(log_public_key, log_id, tree_size, root_hash, timestamp_micros, signature) {
    const ptr0 = passArray8ToWasm0(log_public_key, wasm.__wbindgen_malloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(log_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ptr2 = passArray8ToWasm0(root_hash, wasm.__wbindgen_malloc);
    const len2 = WASM_VECTOR_LEN;
    const ptr3 = passArray8ToWasm0(signature, wasm.__wbindgen_malloc);
    const len3 = WASM_VECTOR_LEN;
    const ret = wasm.verify_tree_head(ptr0, len0, ptr1, len1, tree_size, ptr2, len2, timestamp_micros, ptr3, len3);
    if (ret[2]) {
        throw takeFromExternrefTable0(ret[1]);
    }
    return ret[0] !== 0;
}
function __wbg_get_imports() {
    const import0 = {
        __proto__: null,
        __wbg_Error_30c8987f7c2ed4e2: function(arg0, arg1) {
            const ret = Error(getStringFromWasm0(arg0, arg1));
            return ret;
        },
        __wbg___wbindgen_is_function_1f9d30630b8b1d3d: function(arg0) {
            const ret = typeof(arg0) === 'function';
            return ret;
        },
        __wbg___wbindgen_is_object_3c45d4f2dde4e749: function(arg0) {
            const val = arg0;
            const ret = typeof(val) === 'object' && val !== null;
            return ret;
        },
        __wbg___wbindgen_is_string_90b56bc79aad6f6c: function(arg0) {
            const ret = typeof(arg0) === 'string';
            return ret;
        },
        __wbg___wbindgen_is_undefined_8865fb403f8fe9d8: function(arg0) {
            const ret = arg0 === undefined;
            return ret;
        },
        __wbg___wbindgen_string_get_0380ccaa2f57f0d9: function(arg0, arg1) {
            const obj = arg1;
            const ret = typeof(obj) === 'string' ? obj : undefined;
            var ptr1 = isLikeNone(ret) ? 0 : passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            var len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_throw_41e9ee4f547fc59a: function(arg0, arg1) {
            throw new Error(getStringFromWasm0(arg0, arg1));
        },
        __wbg_call_187d372bd5fdd4aa: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.call(arg1, arg2);
            return ret;
        }, arguments); },
        __wbg_crypto_38df2bab126b63dc: function(arg0) {
            const ret = arg0.crypto;
            return ret;
        },
        __wbg_getRandomValues_436a51d0629d84e1: function() { return handleError(function (arg0, arg1) {
            globalThis.crypto.getRandomValues(getArrayU8FromWasm0(arg0, arg1));
        }, arguments); },
        __wbg_getRandomValues_c44a50d8cfdaebeb: function() { return handleError(function (arg0, arg1) {
            arg0.getRandomValues(arg1);
        }, arguments); },
        __wbg_length_7f3c00c40364105e: function(arg0) {
            const ret = arg0.length;
            return ret;
        },
        __wbg_msCrypto_bd5a034af96bcba6: function(arg0) {
            const ret = arg0.msCrypto;
            return ret;
        },
        __wbg_new_with_length_3da0ad195f6f63ba: function(arg0) {
            const ret = new Uint8Array(arg0 >>> 0);
            return ret;
        },
        __wbg_node_84ea875411254db1: function(arg0) {
            const ret = arg0.node;
            return ret;
        },
        __wbg_now_aa4ccb83129e9e55: function() {
            const ret = Date.now();
            return ret;
        },
        __wbg_process_44c7a14e11e9f69e: function(arg0) {
            const ret = arg0.process;
            return ret;
        },
        __wbg_prototypesetcall_bc27214492979395: function(arg0, arg1, arg2) {
            Uint8Array.prototype.set.call(getArrayU8FromWasm0(arg0, arg1), arg2);
        },
        __wbg_randomFillSync_6c25eac9869eb53c: function() { return handleError(function (arg0, arg1) {
            arg0.randomFillSync(arg1);
        }, arguments); },
        __wbg_require_b4edbdcf3e2a1ef0: function() { return handleError(function () {
            const ret = module.require;
            return ret;
        }, arguments); },
        __wbg_static_accessor_GLOBAL_266715b9d96ba635: function() {
            const ret = typeof global === 'undefined' ? null : global;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_GLOBAL_THIS_10fb7dc1ae063179: function() {
            const ret = typeof globalThis === 'undefined' ? null : globalThis;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_SELF_0b583911f537483a: function() {
            const ret = typeof self === 'undefined' ? null : self;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_WINDOW_d7f903d1508cbdc4: function() {
            const ret = typeof window === 'undefined' ? null : window;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_subarray_002b94d5e13d1411: function(arg0, arg1, arg2) {
            const ret = arg0.subarray(arg1 >>> 0, arg2 >>> 0);
            return ret;
        },
        __wbg_versions_276b2795b1c6a219: function(arg0) {
            const ret = arg0.versions;
            return ret;
        },
        __wbindgen_generic_0000000000000001: function(arg0, arg1) {
            // Cast intrinsic for `Ref(Slice(U8)) -> NamedExternref("Uint8Array")`.
            const ret = getArrayU8FromWasm0(arg0, arg1);
            return ret;
        },
        __wbindgen_generic_0000000000000002: function(arg0, arg1) {
            // Cast intrinsic for `Ref(String) -> Externref`.
            const ret = getStringFromWasm0(arg0, arg1);
            return ret;
        },
        __wbindgen_init_externref_table: function() {
            const table = wasm.__wbindgen_externrefs;
            const offset = table.grow(4);
            table.set(0, undefined);
            table.set(offset + 0, undefined);
            table.set(offset + 1, null);
            table.set(offset + 2, true);
            table.set(offset + 3, false);
        },
    };
    return {
        __proto__: null,
        "./cf_messaging_crypto_wasm_bg.js": import0,
    };
}

const CommitResultFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_commitresult_free(ptr, 1));
const DecryptedFileFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_decryptedfile_free(ptr, 1));
const DescriptorInfoFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_descriptorinfo_free(ptr, 1));
const EncryptedFileFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_encryptedfile_free(ptr, 1));
const KeyPackageInfoFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_keypackageinfo_free(ptr, 1));
const ProcessResultFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_processresult_free(ptr, 1));
const WebDeviceFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_webdevice_free(ptr, 1));

function addToExternrefTable0(obj) {
    const idx = wasm.__externref_table_alloc();
    wasm.__wbindgen_externrefs.set(idx, obj);
    return idx;
}

function getArrayJsValueFromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    const mem = getDataViewMemory0();
    const result = [];
    for (let i = ptr; i < ptr + 4 * len; i += 4) {
        result.push(wasm.__wbindgen_externrefs.get(mem.getUint32(i, true)));
    }
    wasm.__externref_drop_slice(ptr, len);
    return result;
}

function getArrayU8FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getUint8ArrayMemory0().subarray(ptr / 1, ptr / 1 + len);
}

let cachedDataViewMemory0 = null;
function getDataViewMemory0() {
    if (cachedDataViewMemory0 === null || cachedDataViewMemory0.buffer.detached === true || (cachedDataViewMemory0.buffer.detached === undefined && cachedDataViewMemory0.buffer !== wasm.memory.buffer)) {
        cachedDataViewMemory0 = new DataView(wasm.memory.buffer);
    }
    return cachedDataViewMemory0;
}

function getStringFromWasm0(ptr, len) {
    return decodeText(ptr >>> 0, len);
}

let cachedUint8ArrayMemory0 = null;
function getUint8ArrayMemory0() {
    if (cachedUint8ArrayMemory0 === null || cachedUint8ArrayMemory0.byteLength === 0) {
        cachedUint8ArrayMemory0 = new Uint8Array(wasm.memory.buffer);
    }
    return cachedUint8ArrayMemory0;
}

function handleError(f, args) {
    try {
        return f.apply(this, args);
    } catch (e) {
        const idx = addToExternrefTable0(e);
        wasm.__wbindgen_exn_store(idx);
    }
}

function isLikeNone(x) {
    return x === undefined || x === null;
}

function passArray8ToWasm0(arg, malloc) {
    const ptr = malloc(arg.length * 1, 1) >>> 0;
    getUint8ArrayMemory0().set(arg, ptr / 1);
    WASM_VECTOR_LEN = arg.length;
    return ptr;
}

function passArrayJsValueToWasm0(array, malloc) {
    const ptr = malloc(array.length * 4, 4) >>> 0;
    for (let i = 0; i < array.length; i++) {
        const add = addToExternrefTable0(array[i]);
        getDataViewMemory0().setUint32(ptr + 4 * i, add, true);
    }
    WASM_VECTOR_LEN = array.length;
    return ptr;
}

function passStringToWasm0(arg, malloc, realloc) {
    if (realloc === undefined) {
        const buf = cachedTextEncoder.encode(arg);
        const ptr = malloc(buf.length, 1) >>> 0;
        getUint8ArrayMemory0().subarray(ptr, ptr + buf.length).set(buf);
        WASM_VECTOR_LEN = buf.length;
        return ptr;
    }

    let len = arg.length;
    let ptr = malloc(len, 1) >>> 0;

    const mem = getUint8ArrayMemory0();

    let offset = 0;

    for (; offset < len; offset++) {
        const code = arg.charCodeAt(offset);
        if (code > 0x7F) break;
        mem[ptr + offset] = code;
    }
    if (offset !== len) {
        if (offset !== 0) {
            arg = arg.slice(offset);
        }
        ptr = realloc(ptr, len, len = offset + arg.length * 3, 1) >>> 0;
        const view = getUint8ArrayMemory0().subarray(ptr + offset, ptr + len);
        const ret = cachedTextEncoder.encodeInto(arg, view);

        offset += ret.written;
        ptr = realloc(ptr, len, offset, 1) >>> 0;
    }

    WASM_VECTOR_LEN = offset;
    return ptr;
}

function takeFromExternrefTable0(idx) {
    const value = wasm.__wbindgen_externrefs.get(idx);
    wasm.__externref_table_dealloc(idx);
    return value;
}

let cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
cachedTextDecoder.decode();
const MAX_SAFARI_DECODE_BYTES = 2146435072;
let numBytesDecoded = 0;
function decodeText(ptr, len) {
    numBytesDecoded += len;
    if (numBytesDecoded >= MAX_SAFARI_DECODE_BYTES) {
        cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
        cachedTextDecoder.decode();
        numBytesDecoded = len;
    }
    return cachedTextDecoder.decode(getUint8ArrayMemory0().subarray(ptr, ptr + len));
}

const cachedTextEncoder = new TextEncoder();

if (!('encodeInto' in cachedTextEncoder)) {
    cachedTextEncoder.encodeInto = function (arg, view) {
        const buf = cachedTextEncoder.encode(arg);
        view.set(buf);
        return {
            read: arg.length,
            written: buf.length
        };
    };
}

let WASM_VECTOR_LEN = 0;

let wasmModule, wasmInstance, wasm;
function __wbg_finalize_init(instance, module) {
    wasmInstance = instance;
    wasm = instance.exports;
    wasmModule = module;
    cachedDataViewMemory0 = null;
    cachedUint8ArrayMemory0 = null;
    wasm.__wbindgen_start();
    return wasm;
}

async function __wbg_load(module, imports) {
    if (typeof Response === 'function' && module instanceof Response) {
        if (!module.ok) {
            throw new Error(`failed to fetch Wasm: ${module.status} ${module.statusText} fetching '${module.url}'`);
        }

        if (typeof WebAssembly.instantiateStreaming === 'function') {
            try {
                return await WebAssembly.instantiateStreaming(module, imports);
            } catch (e) {
                const validResponse = expectedResponseType(module.type);

                if (validResponse && module.headers.get('Content-Type') !== 'application/wasm') {
                    console.warn("`WebAssembly.instantiateStreaming` failed because your server does not serve Wasm with `application/wasm` MIME type. Falling back to `WebAssembly.instantiate` which is slower. Original error:\n", e);

                } else { throw e; }
            }
        }

        const bytes = await module.arrayBuffer();
        return await WebAssembly.instantiate(bytes, imports);
    } else {
        const instance = await WebAssembly.instantiate(module, imports);

        if (instance instanceof WebAssembly.Instance) {
            return { instance, module };
        } else {
            return instance;
        }
    }

    function expectedResponseType(type) {
        switch (type) {
            case 'basic': case 'cors': case 'default': return true;
        }
        return false;
    }
}

function initSync(module) {
    if (wasm !== undefined) return wasm;


    if (module !== undefined) {
        if (Object.getPrototypeOf(module) === Object.prototype) {
            ({module} = module)
        } else {
            console.warn('using deprecated parameters for `initSync()`; pass a single object instead')
        }
    }

    const imports = __wbg_get_imports();
    if (!(module instanceof WebAssembly.Module)) {
        module = new WebAssembly.Module(module);
    }
    const instance = new WebAssembly.Instance(module, imports);
    return __wbg_finalize_init(instance, module);
}

async function __wbg_init(module_or_path) {
    if (wasm !== undefined) return wasm;


    if (module_or_path !== undefined) {
        if (Object.getPrototypeOf(module_or_path) === Object.prototype) {
            ({module_or_path} = module_or_path)
        } else {
            console.warn('using deprecated parameters for the initialization function; pass a single object instead')
        }
    }

    if (module_or_path === undefined) {
        module_or_path = new URL('cf_messaging_crypto_wasm_bg.wasm', import.meta.url);
    }
    const imports = __wbg_get_imports();

    if (typeof module_or_path === 'string' || (typeof Request === 'function' && module_or_path instanceof Request) || (typeof URL === 'function' && module_or_path instanceof URL)) {
        module_or_path = fetch(module_or_path);
    }

    const { instance, module } = await __wbg_load(await module_or_path, imports);

    return __wbg_finalize_init(instance, module);
}

export { initSync, __wbg_init as default };
