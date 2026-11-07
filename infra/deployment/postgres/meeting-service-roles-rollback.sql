-- SEC-018 rollback of meeting-service-roles-cutover.sql (docs/operations/meeting-service-database-roles.md).
--
-- Returns every object the MIGRATOR role owns in schema public of the meeting-service
-- database to the PREVIOUS OWNER (by default the runtime role; in branch B the role that
-- owned them before, e.g. the old superuser login), returns the database to the owner
-- recorded in the pre-checks, gives the previous owner CREATE on schema public back and
-- removes the migrator's default privileges. Explicit grants to the runtime role stay
-- (harmless). One transaction; refuses to commit while the migrator still owns anything in
-- this database.
--
-- OWNER ACTION, as a superuser connected to the meeting-service database, AFTER removing the
-- release's migrator URL (so releases migrate as the previous owner again):
--   psql -X -d <database> -v runtime=<runtime role> -v migrator=<migrator role> \
--        -v database=<database> -v database_owner=<database owner recorded before the cutover> \
--        [-v owner=<object owner recorded before the cutover>] [-v dry_run=1] \
--        -f meeting-service-roles-rollback.sql
-- Rehearsed in CI by verify-meeting-service-roles.sh --rehearse-cutover[-from-superuser].
\set ON_ERROR_STOP on
\set QUIET on
\if :{?owner}
\else
\set owner :runtime
\endif
SET lock_timeout = '5s';
BEGIN;
-- Role and database names reach the DO blocks below through transaction-local settings.
SELECT set_config('cfmeet.migrator', :'migrator', true), set_config('cfmeet.database', :'database', true),
       set_config('cfmeet.database_owner', :'database_owner', true), set_config('cfmeet.owner', :'owner', true) \gset sec018_
DO $$
BEGIN
  IF current_database() <> current_setting('cfmeet.database') THEN
    RAISE EXCEPTION 'connected to %, not the meeting-service database %', current_database(), current_setting('cfmeet.database');
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = current_setting('cfmeet.database_owner'))
     OR NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = current_setting('cfmeet.owner')) THEN
    RAISE EXCEPTION 'recorded owner % / database owner % must exist', current_setting('cfmeet.owner'), current_setting('cfmeet.database_owner');
  END IF;
  IF current_setting('cfmeet.owner') = current_setting('cfmeet.migrator') THEN
    RAISE EXCEPTION 'the previous owner cannot be the migrator role';
  END IF;
END $$;

\set ECHO queries
SELECT format('ALTER DATABASE %I OWNER TO %I', :'database', :'database_owner') \gexec
SELECT format('ALTER SCHEMA public OWNER TO %I', :'owner')
  FROM pg_namespace WHERE nspname = 'public' AND nspowner = (SELECT oid FROM pg_roles WHERE rolname = :'migrator') \gexec
SELECT format('ALTER %s %I.%I OWNER TO %I',
         CASE c.relkind WHEN 'v' THEN 'VIEW' WHEN 'm' THEN 'MATERIALIZED VIEW' WHEN 'S' THEN 'SEQUENCE'
                        WHEN 'f' THEN 'FOREIGN TABLE' WHEN 'c' THEN 'TYPE' ELSE 'TABLE' END,
         n.nspname, c.relname, :'owner')
  FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
 WHERE n.nspname = 'public' AND c.relowner = (SELECT oid FROM pg_roles WHERE rolname = :'migrator')
   AND c.relkind IN ('r', 'p', 'v', 'm', 'f', 'S', 'c')
   AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_class'::regclass AND d.objid = c.oid
                      AND (d.deptype = 'e' OR (c.relkind = 'S' AND d.refclassid = 'pg_class'::regclass AND d.deptype IN ('a', 'i'))))
 ORDER BY c.relkind, c.relname \gexec
SELECT format('ALTER ROUTINE %s OWNER TO %I', p.oid::regprocedure, :'owner')
  FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
 WHERE n.nspname = 'public' AND p.proowner = (SELECT oid FROM pg_roles WHERE rolname = :'migrator')
   AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_proc'::regclass AND d.objid = p.oid AND d.deptype = 'e')
 ORDER BY 1 \gexec
SELECT format('ALTER %s %s OWNER TO %I', CASE t.typtype WHEN 'd' THEN 'DOMAIN' ELSE 'TYPE' END, t.oid::regtype, :'owner')
  FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace
 WHERE n.nspname = 'public' AND t.typowner = (SELECT oid FROM pg_roles WHERE rolname = :'migrator')
   AND t.typtype IN ('e', 'd', 'r')
   AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_type'::regclass AND d.objid = t.oid AND d.deptype = 'e')
 ORDER BY 1 \gexec
-- Releases migrate as the previous owner again, which therefore needs CREATE on the schema.
SELECT format('GRANT CREATE ON SCHEMA public TO %I', :'owner') \gexec
SELECT format('REVOKE ALL ON SCHEMA public FROM %I', :'migrator') \gexec
SELECT format('ALTER DEFAULT PRIVILEGES FOR ROLE %I IN SCHEMA public REVOKE ALL ON TABLES FROM %I', :'migrator', :'runtime') \gexec
SELECT format('ALTER DEFAULT PRIVILEGES FOR ROLE %I IN SCHEMA public REVOKE ALL ON SEQUENCES FROM %I', :'migrator', :'runtime') \gexec
SELECT format('ALTER DEFAULT PRIVILEGES FOR ROLE %I IN SCHEMA public REVOKE ALL ON FUNCTIONS FROM %I', :'migrator', :'runtime') \gexec
\set ECHO none

DO $$
DECLARE mg name := current_setting('cfmeet.migrator'); left_over text;
BEGIN
  SELECT string_agg(pg_describe_object(classid, objid, objsubid), ', ') INTO left_over
    FROM pg_shdepend
   WHERE refclassid = 'pg_authid'::regclass AND refobjid = (SELECT oid FROM pg_roles WHERE rolname = mg) AND deptype = 'o'
     AND (dbid = (SELECT oid FROM pg_database WHERE datname = current_database())
          OR (classid = 'pg_database'::regclass AND objid = (SELECT oid FROM pg_database WHERE datname = current_database())));
  IF left_over IS NOT NULL THEN
    RAISE EXCEPTION 'migrator role % still owns: % (nothing committed)', mg, left_over;
  END IF;
END $$;

\if :{?dry_run}
ROLLBACK;
\echo 'meeting-service role rollback: DRY RUN rolled back'
\else
COMMIT;
\echo 'meeting-service role rollback: committed'
\endif
