/*
 * CF Meet Messaging client crypto core: C ABI (cf-messaging-crypto-ffi, ABI version 1).
 *
 * Hand-maintained mirror of packages/messaging-crypto/ffi/src/lib.rs, which documents
 * every function. Rules:
 *   - every function returns a cfm_status (CFM_OK or an error class); on error
 *     cfm_last_error() returns the stable error code (thread-local, valid until the next
 *     call on the same thread). Codes never contain key material or caller data;
 *   - inputs are (const uint8_t *ptr, size_t len); ptr may be NULL only when len == 0;
 *     strings are UTF-8, not NUL-terminated;
 *   - output buffers belong to the caller: release with cfm_buffer_free (zeroizes);
 *   - handles (cfm_device, cfm_encrypted_file) are released with their *_free function;
 *   - no output is written when a call fails.
 */
#ifndef CF_MESSAGING_CRYPTO_H
#define CF_MESSAGING_CRYPTO_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define CFM_ABI_VERSION 1u

typedef int32_t cfm_status;
#define CFM_OK 0
#define CFM_ERROR 1            /* refused: code in cfm_last_error() */
#define CFM_INVALID_ARGUMENT 2 /* NULL pointer / impossible length / NULL handle */
#define CFM_PANIC 3            /* internal panic caught, or handle poisoned by one */

#define CFM_PROCESSED_APPLICATION 1u
#define CFM_PROCESSED_COMMIT 2u
#define CFM_PROCESSED_PROPOSAL 3u

typedef struct cfm_device cfm_device;
typedef struct cfm_encrypted_file cfm_encrypted_file;

typedef struct {
  uint8_t *ptr;
  size_t len;
} cfm_buffer;

typedef struct {
  const uint8_t *ptr;
  size_t len;
} cfm_bytes;

typedef struct {
  cfm_buffer commit;
  cfm_buffer welcome; /* empty unless has_welcome */
  uint8_t has_welcome;
  uint64_t epoch;
} cfm_commit_result;

typedef struct {
  uint32_t kind; /* CFM_PROCESSED_* */
  cfm_buffer plaintext;
  uint64_t epoch;
} cfm_process_result;

uint32_t cfm_abi_version(void);
const char *cfm_last_error(void);
void cfm_buffer_free(cfm_buffer buf);

cfm_status cfm_message_aad(const uint8_t *tenant, size_t tenant_len,
                           const uint8_t *application, size_t application_len,
                           const uint8_t *environment, size_t environment_len,
                           const uint8_t *conversation, size_t conversation_len,
                           const uint8_t *client_message, size_t client_message_len,
                           const uint8_t *sender_device, size_t sender_device_len,
                           cfm_buffer *out);
cfm_status cfm_inspect_key_package(const uint8_t *encoded, size_t encoded_len, uint64_t now,
                                   cfm_buffer *out_device_id, cfm_buffer *out_signature_key);
cfm_status cfm_verify_tree_head(const uint8_t *log_public_key, size_t log_public_key_len,
                                const uint8_t *log_id, size_t log_id_len, uint64_t tree_size,
                                const uint8_t *root_hash, size_t root_hash_len,
                                int64_t timestamp_micros, const uint8_t *signature,
                                size_t signature_len, uint8_t *out_valid);
cfm_status cfm_verify_inclusion(uint64_t index, uint64_t size, const uint8_t *leaf_data,
                                size_t leaf_data_len, const uint8_t *proof, size_t proof_len,
                                const uint8_t *root, size_t root_len, uint8_t *out_valid);
cfm_status cfm_verify_consistency(uint64_t old_size, uint64_t new_size, const uint8_t *old_root,
                                  size_t old_root_len, const uint8_t *new_root,
                                  size_t new_root_len, const uint8_t *proof, size_t proof_len,
                                  uint8_t *out_valid);

cfm_status cfm_encrypt_file(const uint8_t *plaintext, size_t plaintext_len,
                            const uint8_t *media_type, size_t media_type_len,
                            const uint8_t *file_name, size_t file_name_len,
                            cfm_encrypted_file **out);
cfm_status cfm_encrypted_file_ciphertext(const cfm_encrypted_file *file, cfm_buffer *out);
cfm_status cfm_encrypted_file_sha256(const cfm_encrypted_file *file, cfm_buffer *out);
cfm_status cfm_encrypted_file_size(const cfm_encrypted_file *file, uint64_t *out);
cfm_status cfm_encrypted_file_descriptor_for(const cfm_encrypted_file *file,
                                             const uint8_t *attachment_id,
                                             size_t attachment_id_len, cfm_buffer *out);
void cfm_encrypted_file_free(cfm_encrypted_file *file);
cfm_status cfm_decrypt_file(const uint8_t *descriptor, size_t descriptor_len,
                            const uint8_t *attachment_id, size_t attachment_id_len,
                            const uint8_t *ciphertext, size_t ciphertext_len,
                            cfm_buffer *out_plaintext, cfm_buffer *out_media_type,
                            cfm_buffer *out_file_name);
cfm_status cfm_descriptor_info(const uint8_t *descriptor, size_t descriptor_len,
                               cfm_buffer *out_media_type, cfm_buffer *out_file_name,
                               uint64_t *out_size);
cfm_status cfm_is_descriptor(const uint8_t *bytes, size_t bytes_len, uint8_t *out);

cfm_status cfm_verify_transfer_request(const uint8_t *target_device_key,
                                       size_t target_device_key_len, const cfm_bytes *ctx,
                                       size_t ctx_count, const uint8_t *transfer_key,
                                       size_t transfer_key_len, int64_t expires_at,
                                       const uint8_t *signature, size_t signature_len,
                                       uint8_t *out_valid);
cfm_status cfm_comparison_code(const cfm_bytes *ctx, size_t ctx_count,
                               const uint8_t *transfer_key, size_t transfer_key_len,
                               const uint8_t *source_key, size_t source_key_len,
                               const uint8_t *target_key, size_t target_key_len,
                               cfm_buffer *out);

cfm_status cfm_device_new(const uint8_t *device_id, size_t device_id_len, cfm_device **out);
cfm_status cfm_device_from_state(const uint8_t *device_id, size_t device_id_len,
                                 const uint8_t *state, size_t state_len, cfm_device **out);
cfm_status cfm_device_export_state(const cfm_device *device, cfm_buffer *out); /* SECRET */
cfm_status cfm_device_id(const cfm_device *device, cfm_buffer *out);
cfm_status cfm_device_signature_public_key(const cfm_device *device, cfm_buffer *out);
cfm_status cfm_device_key_package(const cfm_device *device, cfm_buffer *out);
cfm_status cfm_device_registration_key_package(const cfm_device *device,
                                               const uint8_t *challenge_id,
                                               size_t challenge_id_len, const uint8_t *nonce,
                                               size_t nonce_len, cfm_buffer *out);
cfm_status cfm_device_has_group(const cfm_device *device, const uint8_t *group_id,
                                size_t group_id_len, uint8_t *out);
cfm_status cfm_device_create_group(const cfm_device *device, const uint8_t *group_id,
                                   size_t group_id_len, uint64_t *out_epoch);
cfm_status cfm_device_join(const cfm_device *device, const uint8_t *group_id,
                           size_t group_id_len, const uint8_t *welcome, size_t welcome_len,
                           uint64_t *out_epoch);
cfm_status cfm_device_add_devices(const cfm_device *device, const uint8_t *group_id,
                                  size_t group_id_len, const uint8_t *packed, size_t packed_len,
                                  cfm_commit_result *out);
cfm_status cfm_device_remove_devices(const cfm_device *device, const uint8_t *group_id,
                                     size_t group_id_len, const cfm_bytes *device_ids,
                                     size_t device_ids_count, cfm_commit_result *out);
cfm_status cfm_device_encrypt(const cfm_device *device, const uint8_t *group_id,
                              size_t group_id_len, const uint8_t *aad, size_t aad_len,
                              const uint8_t *plaintext, size_t plaintext_len, cfm_buffer *out);
cfm_status cfm_device_process(const cfm_device *device, const uint8_t *group_id,
                              size_t group_id_len, const uint8_t *message, size_t message_len,
                              const uint8_t *expected_aad, size_t expected_aad_len,
                              uint8_t has_expected_aad, cfm_process_result *out);
cfm_status cfm_device_epoch(const cfm_device *device, const uint8_t *group_id,
                            size_t group_id_len, uint64_t *out);
cfm_status cfm_device_members(const cfm_device *device, const uint8_t *group_id,
                              size_t group_id_len, cfm_buffer *out);
cfm_status cfm_device_begin_transfer(const cfm_device *device, cfm_buffer *out);
cfm_status cfm_device_sign_transfer_request(const cfm_device *device, const cfm_bytes *ctx,
                                            size_t ctx_count, int64_t expires_at,
                                            cfm_buffer *out);
cfm_status cfm_device_seal_history(const cfm_device *device, const cfm_bytes *ctx,
                                   size_t ctx_count, const uint8_t *transfer_key,
                                   size_t transfer_key_len, const uint8_t *history,
                                   size_t history_len, cfm_buffer *out);
cfm_status cfm_device_open_history(const cfm_device *device, const uint8_t *source_key,
                                   size_t source_key_len, const cfm_bytes *ctx,
                                   size_t ctx_count, const uint8_t *sealed, size_t sealed_len,
                                   cfm_buffer *out);
void cfm_device_free(cfm_device *device);

#ifdef __cplusplus
}
#endif

#endif /* CF_MESSAGING_CRYPTO_H */
