-- Admin bridge least-privilege database roles (ADR 0068, design §2 / §13).
--
-- Database cfmeet_admin_bridge with two roles:
--   admin_bridge_migrator  owns the database, schema objects, trigger functions and the
--                          audit tables; used only by admin-bridge-migration in a release.
--   admin_bridge_runtime   the role the bridge connects with. NOSUPERUSER NOCREATEDB
--                          NOCREATEROLE NOREPLICATION NOBYPASSRLS, owns nothing, no DDL,
--                          no TRUNCATE anywhere, and only the table privileges below:
--                            SELECT, INSERT                      every admin_ table
--                            UPDATE (revoked_at)                 admin_remote_principals
--                            UPDATE                              admin_rate_limits,
--                                                                admin_dataset_days,
--                                                                admin_projections,
--                                                                admin_operations (its
--                                                                trigger allows only the
--                                                                send-once transitions)
--                            DELETE                              admin_security_claims,
--                                                                admin_rate_limits (cleanup)
--                          The append-only tables (audit, revocations, policy revisions)
--                          get no UPDATE / DELETE privilege on top of their triggers.
--
-- OWNER ACTION, two steps (passwords never pass through this file):
--   1. as a superuser on the maintenance database, create roles and database:
--        psql -X -d postgres -v step=roles -v migrator_password=... -v runtime_password=... \
--             -f db-roles.sql
--      (or pass the passwords from files: -v migrator_password="$(cat F)")
--   2. after `admin-bridge-migration up` has run as admin_bridge_migrator, connected to
--      cfmeet_admin_bridge as a superuser (or the migrator), apply the grants:
--        psql -X -d cfmeet_admin_bridge -v step=grants -f db-roles.sql
-- Both steps are idempotent; step 2 re-runs after every migration that adds a table.
\set ON_ERROR_STOP on
\set QUIET on

\if :{?step}
\else
\echo 'db-roles.sql: pass -v step=roles or -v step=grants'
\quit
\endif

SELECT :'step' = 'roles' AS step_roles, :'step' = 'grants' AS step_grants \gset

\if :step_roles
SELECT 'CREATE ROLE admin_bridge_migrator LOGIN NOINHERIT'
 WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'admin_bridge_migrator') \gexec
SELECT 'CREATE ROLE admin_bridge_runtime LOGIN NOINHERIT'
 WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'admin_bridge_runtime') \gexec
SELECT format('ALTER ROLE admin_bridge_migrator PASSWORD %L NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS', :'migrator_password') \gexec
SELECT format('ALTER ROLE admin_bridge_runtime PASSWORD %L NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS', :'runtime_password') \gexec
SELECT 'CREATE DATABASE cfmeet_admin_bridge OWNER admin_bridge_migrator'
 WHERE NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = 'cfmeet_admin_bridge') \gexec
REVOKE ALL ON DATABASE cfmeet_admin_bridge FROM PUBLIC;
GRANT CONNECT ON DATABASE cfmeet_admin_bridge TO admin_bridge_migrator, admin_bridge_runtime;
\echo 'db-roles.sql: roles and database ready; run admin-bridge-migration up, then step=grants'
\endif

\if :step_grants
BEGIN;
DO $$
BEGIN
    IF current_database() <> 'cfmeet_admin_bridge' THEN
        RAISE EXCEPTION 'connected to %, not cfmeet_admin_bridge', current_database();
    END IF;
    IF EXISTS (SELECT 1 FROM pg_tables WHERE schemaname = 'public'
               AND tablename LIKE 'admin\_%' AND tableowner <> 'admin_bridge_migrator') THEN
        RAISE EXCEPTION 'every admin_ table must be owned by admin_bridge_migrator';
    END IF;
END $$;

REVOKE CREATE ON SCHEMA public FROM PUBLIC;
REVOKE ALL ON SCHEMA public FROM admin_bridge_runtime;
GRANT USAGE ON SCHEMA public TO admin_bridge_runtime;
REVOKE TEMPORARY ON DATABASE cfmeet_admin_bridge FROM PUBLIC;

-- Start from nothing, then grant exactly what the bridge needs.
REVOKE ALL ON ALL TABLES IN SCHEMA public FROM admin_bridge_runtime;
REVOKE ALL ON ALL SEQUENCES IN SCHEMA public FROM admin_bridge_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE admin_bridge_migrator IN SCHEMA public REVOKE ALL ON TABLES FROM admin_bridge_runtime;

GRANT SELECT, INSERT ON
    admin_remote_principals, admin_security_claims, admin_revocations, admin_rate_limits,
    admin_policy_revisions, admin_audit_entries, admin_dataset_days, admin_projections,
    admin_operations, admin_privacy_requests, admin_privacy_request_services
    TO admin_bridge_runtime;
GRANT UPDATE (revoked_at) ON admin_remote_principals TO admin_bridge_runtime;
GRANT UPDATE ON admin_rate_limits, admin_dataset_days, admin_projections, admin_operations,
    admin_privacy_requests, admin_privacy_request_services TO admin_bridge_runtime;
GRANT DELETE ON admin_security_claims, admin_rate_limits TO admin_bridge_runtime;
-- Identity columns (audit sequence, revocation id).
GRANT USAGE ON ALL SEQUENCES IN SCHEMA public TO admin_bridge_runtime;
GRANT SELECT ON admin_bridge_seaql_migrations TO admin_bridge_runtime;

-- Refuse to commit if the runtime role could change history or the schema.
DO $$
DECLARE bad TEXT;
BEGIN
    SELECT string_agg(table_name || ':' || privilege_type, ', ') INTO bad
      FROM information_schema.role_table_grants
     WHERE grantee = 'admin_bridge_runtime'
       AND (privilege_type IN ('TRUNCATE', 'REFERENCES', 'TRIGGER')
            OR (privilege_type IN ('UPDATE', 'DELETE')
                AND table_name IN ('admin_audit_entries', 'admin_revocations', 'admin_policy_revisions'))
            OR (privilege_type = 'DELETE' AND table_name = 'admin_operations'));
    IF bad IS NOT NULL THEN
        RAISE EXCEPTION 'admin_bridge_runtime has forbidden privileges: %', bad;
    END IF;
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'admin_bridge_runtime'
               AND (rolsuper OR rolcreatedb OR rolcreaterole OR rolbypassrls OR rolreplication)) THEN
        RAISE EXCEPTION 'admin_bridge_runtime must not be superuser or create objects';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_class c JOIN pg_roles r ON r.oid = c.relowner
               WHERE r.rolname = 'admin_bridge_runtime') THEN
        RAISE EXCEPTION 'admin_bridge_runtime must own nothing';
    END IF;
END $$;
COMMIT;
\echo 'db-roles.sql: runtime grants applied'
\endif
