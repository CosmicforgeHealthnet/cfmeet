-- SEC-018 meeting-service least-privilege cutover (docs/operations/meeting-service-database-roles.md).
--
-- Moves every object the CURRENT OWNER (by default the runtime role) owns in schema public of
-- the meeting-service database (tables with their indexes and owned sequences, views,
-- standalone sequences, functions incl. trigger functions, enum / domain / range / composite
-- types) and the database itself to the MIGRATOR role; gives the RUNTIME role data access
-- only (DML, sequences, functions, default privileges for what the migrator creates later);
-- removes CREATE / TEMPORARY from the runtime role and from PUBLIC. One transaction: all or
-- nothing. It refuses to commit unless every non-extension table, view, sequence, function
-- and type in schema public belongs to the migrator (indexes and column-owned sequences
-- follow their table) and the runtime role owns nothing in this database. Per-object
-- ALTER ... OWNER, not REASSIGN OWNED, so no other database and nothing outside this
-- database is touched.
--
-- OWNER ACTION, as a superuser connected to the meeting-service database; the migrator (and,
-- in branch B, the new runtime) role must already exist (created separately: passwords never
-- pass through this file):
--   psql -X -d <database> -v runtime=<runtime role> -v migrator=<migrator role> \
--        -v database=<database> [-v current_owner=<role owning the objects today>] \
--        [-v dry_run=1] -f meeting-service-roles-cutover.sql
-- dry_run=1 performs everything and then ROLLS BACK (it still takes the brief locks).
-- Rehearsed in CI by verify-meeting-service-roles.sh --rehearse-cutover[-from-superuser].
\set ON_ERROR_STOP on
\set QUIET on
\if :{?current_owner}
\else
\set current_owner :runtime
\endif
SET lock_timeout = '5s';
BEGIN;
-- Role and database names reach the DO blocks below through transaction-local settings.
SELECT set_config('cfmeet.runtime', :'runtime', true), set_config('cfmeet.migrator', :'migrator', true),
       set_config('cfmeet.database', :'database', true), set_config('cfmeet.current_owner', :'current_owner', true) \gset sec018_
DO $$
DECLARE rt name := current_setting('cfmeet.runtime'); mg name := current_setting('cfmeet.migrator');
        cur name := current_setting('cfmeet.current_owner');
BEGIN
  IF current_database() <> current_setting('cfmeet.database') THEN
    RAISE EXCEPTION 'connected to %, not the meeting-service database %', current_database(), current_setting('cfmeet.database');
  END IF;
  IF rt = mg OR cur = mg THEN RAISE EXCEPTION 'the migrator role must differ from the runtime role and the current owner'; END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = mg AND rolcanlogin AND NOT rolsuper) THEN
    RAISE EXCEPTION 'migrator role % must exist, LOGIN, NOSUPERUSER', mg;
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = rt AND rolcanlogin AND NOT rolsuper AND NOT rolbypassrls) THEN
    RAISE EXCEPTION 'runtime role % must exist and must not be a superuser (see the runbook, branch B)', rt;
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = cur) THEN
    RAISE EXCEPTION 'current owner % does not exist', cur;
  END IF;
END $$;

\set ECHO queries
SELECT format('ALTER DATABASE %I OWNER TO %I', :'database', :'migrator') \gexec
-- PostgreSQL >= 15: public belongs to pg_database_owner (now the migrator). Older: explicit.
SELECT format('ALTER SCHEMA public OWNER TO %I', :'migrator')
  FROM pg_namespace WHERE nspname = 'public' AND nspowner = (SELECT oid FROM pg_roles WHERE rolname = :'current_owner') \gexec
SELECT format('GRANT USAGE, CREATE ON SCHEMA public TO %I', :'migrator') \gexec
-- Indexes, TOAST tables and sequences OWNED BY a column follow their table.
SELECT format('ALTER %s %I.%I OWNER TO %I',
         CASE c.relkind WHEN 'v' THEN 'VIEW' WHEN 'm' THEN 'MATERIALIZED VIEW' WHEN 'S' THEN 'SEQUENCE'
                        WHEN 'f' THEN 'FOREIGN TABLE' WHEN 'c' THEN 'TYPE' ELSE 'TABLE' END,
         n.nspname, c.relname, :'migrator')
  FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
 WHERE n.nspname = 'public' AND c.relowner = (SELECT oid FROM pg_roles WHERE rolname = :'current_owner')
   AND c.relkind IN ('r', 'p', 'v', 'm', 'f', 'S', 'c')
   AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_class'::regclass AND d.objid = c.oid
                      AND (d.deptype = 'e' OR (c.relkind = 'S' AND d.refclassid = 'pg_class'::regclass AND d.deptype IN ('a', 'i'))))
 ORDER BY c.relkind, c.relname \gexec
SELECT format('ALTER ROUTINE %s OWNER TO %I', p.oid::regprocedure, :'migrator')
  FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
 WHERE n.nspname = 'public' AND p.proowner = (SELECT oid FROM pg_roles WHERE rolname = :'current_owner')
   AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_proc'::regclass AND d.objid = p.oid AND d.deptype = 'e')
 ORDER BY 1 \gexec
-- Enum (user_status, user_role, auth_type, ...), domain and range types. Array, table row and
-- multirange types follow their base object.
SELECT format('ALTER %s %s OWNER TO %I', CASE t.typtype WHEN 'd' THEN 'DOMAIN' ELSE 'TYPE' END, t.oid::regtype, :'migrator')
  FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace
 WHERE n.nspname = 'public' AND t.typowner = (SELECT oid FROM pg_roles WHERE rolname = :'current_owner')
   AND t.typtype IN ('e', 'd', 'r')
   AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_type'::regclass AND d.objid = t.oid AND d.deptype = 'e')
 ORDER BY 1 \gexec
\set ECHO none

-- The runtime role keeps exactly the data access it uses, now as explicit grants.
SELECT format('GRANT CONNECT ON DATABASE %I TO %I', :'database', :'runtime') \gexec
SELECT format('GRANT USAGE ON SCHEMA public TO %I', :'runtime') \gexec
SELECT format('GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO %I', :'runtime') \gexec
SELECT format('GRANT USAGE, SELECT, UPDATE ON ALL SEQUENCES IN SCHEMA public TO %I', :'runtime') \gexec
SELECT format('GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA public TO %I', :'runtime') \gexec
SELECT format('ALTER DEFAULT PRIVILEGES FOR ROLE %I IN SCHEMA public GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO %I', :'migrator', :'runtime') \gexec
SELECT format('ALTER DEFAULT PRIVILEGES FOR ROLE %I IN SCHEMA public GRANT USAGE, SELECT, UPDATE ON SEQUENCES TO %I', :'migrator', :'runtime') \gexec
SELECT format('ALTER DEFAULT PRIVILEGES FOR ROLE %I IN SCHEMA public GRANT EXECUTE ON FUNCTIONS TO %I', :'migrator', :'runtime') \gexec
-- No DDL path remains: no CREATE on the schema, no CREATE / TEMPORARY on the database.
SELECT format('REVOKE CREATE ON SCHEMA public FROM PUBLIC, %I', :'runtime') \gexec
SELECT format('REVOKE CREATE, TEMPORARY ON DATABASE %I FROM PUBLIC, %I', :'database', :'runtime') \gexec

DO $$
DECLARE rt name := current_setting('cfmeet.runtime'); mg oid := (SELECT oid FROM pg_roles WHERE rolname = current_setting('cfmeet.migrator'));
        db name := current_setting('cfmeet.database'); left_over text;
BEGIN
  SELECT string_agg(o, ', ') INTO left_over FROM (
    SELECT format('relation %s', c.oid::regclass) AS o FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
     WHERE n.nspname = 'public' AND c.relowner <> mg AND c.relkind IN ('r', 'p', 'v', 'm', 'f', 'S', 'c')
       AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_class'::regclass AND d.objid = c.oid
                          AND (d.deptype = 'e' OR (c.relkind = 'S' AND d.refclassid = 'pg_class'::regclass AND d.deptype IN ('a', 'i'))))
    UNION ALL
    SELECT format('function %s', p.oid::regprocedure) FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
     WHERE n.nspname = 'public' AND p.proowner <> mg
       AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_proc'::regclass AND d.objid = p.oid AND d.deptype = 'e')
    UNION ALL
    SELECT format('type %s', t.oid::regtype) FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace
     WHERE n.nspname = 'public' AND t.typowner <> mg AND t.typtype IN ('e', 'd', 'r')
       AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_type'::regclass AND d.objid = t.oid AND d.deptype = 'e')
    UNION ALL
    SELECT pg_describe_object(classid, objid, objsubid) FROM pg_shdepend
     WHERE refclassid = 'pg_authid'::regclass AND refobjid = (SELECT oid FROM pg_roles WHERE rolname = rt) AND deptype = 'o'
       AND (dbid = (SELECT oid FROM pg_database WHERE datname = current_database())
            OR (classid = 'pg_database'::regclass AND objid = (SELECT oid FROM pg_database WHERE datname = db)))
  ) x;
  IF left_over IS NOT NULL THEN
    RAISE EXCEPTION 'not owned by the migrator yet: % (nothing committed)', left_over;
  END IF;
  IF has_schema_privilege(rt, 'public', 'CREATE') OR has_database_privilege(rt, db, 'CREATE')
     OR has_database_privilege(rt, db, 'TEMPORARY') THEN
    RAISE EXCEPTION 'runtime role % still has CREATE or TEMPORARY (nothing committed)', rt;
  END IF;
END $$;

\if :{?dry_run}
ROLLBACK;
\echo 'meeting-service role cutover: DRY RUN rolled back'
\else
COMMIT;
\echo 'meeting-service role cutover: committed'
\endif
