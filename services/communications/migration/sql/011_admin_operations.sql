-- ADR 0068 phase A4 (admin bridge commands): the operations ledger of the commands the HQ
-- admin bridge (cfmeet.admin-bridge, verified `hq:<staff>` attribution) had APPLIED here.
-- Additive only: the previous image never names this table and checks its own histories.
--
-- * one row per applied command, written in the SAME transaction as the change and keyed
--   by HQ's Idempotency-Key, so the bridge settles an unknown outcome from it
--   (GET /internal/v1/operator/operations/{key}); a key that is not here was never applied;
-- * who acted (operator_ref) and, for the R3 commands (profile suspend / restore, report
--   review), who approved it: a different person, enforced here as the second guard;
-- * resulting_state / resulting_version are what the command answered (version NULL for
--   unversioned subjects); resulting_resource is the answer of the unversioned commands
--   (backfill, replay: ids and counts only), so a repeated key returns it without
--   applying again;
-- * append-only: never updated or deleted.
CREATE TABLE IF NOT EXISTS communication_admin_operations (
 idempotency_key UUID PRIMARY KEY,
 command VARCHAR(48) NOT NULL CHECK (command IN ('PROFILE_SUSPEND','PROFILE_RESTORE','REPORT_REVIEW','BACKFILL_RUN','EVENTS_REPLAY')),
 subject_kind VARCHAR(24) NOT NULL CHECK (subject_kind IN ('PROFILE','REPORT','ENVIRONMENT','OUTBOX_EVENT')),
 subject_id UUID NOT NULL,
 operator_ref VARCHAR(160) NOT NULL CHECK (operator_ref ~ '^hq:[A-Za-z0-9._:@-]{1,128}$'),
 approver_ref VARCHAR(160),
 approval_ref VARCHAR(128),
 resulting_state VARCHAR(24) NOT NULL CHECK (resulting_state ~ '^[A-Z][A-Z_]{1,23}$'),
 resulting_version BIGINT CHECK (resulting_version IS NULL OR resulting_version > 0),
 resulting_resource JSONB NOT NULL DEFAULT '{}'::jsonb,
 recorded_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 CONSTRAINT ck_communication_admin_operations_approver CHECK (
   (approver_ref IS NULL) = (approval_ref IS NULL)
   AND (approver_ref IS NULL OR (approver_ref ~ '^hq:[A-Za-z0-9._:@-]{1,128}$' AND approver_ref <> operator_ref))
 ),
 CONSTRAINT ck_communication_admin_operations_two_person CHECK (
   command NOT IN ('PROFILE_SUSPEND','PROFILE_RESTORE','REPORT_REVIEW') OR approver_ref IS NOT NULL
 )
);
CREATE INDEX IF NOT EXISTS communication_admin_operations_subject
 ON communication_admin_operations (subject_kind, subject_id, recorded_at);
CREATE OR REPLACE FUNCTION communication_admin_operations_append_only() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 RAISE EXCEPTION 'communication_admin_operations is append-only' USING ERRCODE='integrity_constraint_violation';
END; $$;
DROP TRIGGER IF EXISTS communication_admin_operations_immutable ON communication_admin_operations;
CREATE TRIGGER communication_admin_operations_immutable BEFORE UPDATE OR DELETE ON communication_admin_operations
 FOR EACH ROW EXECUTE FUNCTION communication_admin_operations_append_only();
DROP TRIGGER IF EXISTS communication_admin_operations_no_truncate ON communication_admin_operations;
CREATE TRIGGER communication_admin_operations_no_truncate BEFORE TRUNCATE ON communication_admin_operations
 FOR EACH STATEMENT EXECUTE FUNCTION communication_admin_operations_append_only();
