-- DATA-001 (Blueprint v1.0 Section 16) — READ-ONLY pre-check for the legacy
-- TIMESTAMP -> TIMESTAMPTZ conversion. Operator-run with psql against the meeting-service
-- database; it changes nothing and is never run by any migrator, release script or CI.
--
--   psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -f data-001-timestamptz-precheck.sql
--
-- It reports (1) the session time zone and the server default, (2) every naive
-- `timestamp without time zone` column outside the two intentional local wall-clock
-- columns, with table size and row estimate (the rewrite and lock cost), (3) column
-- defaults, and (4) the conversion statements as TEXT for review. Do NOT execute the
-- generated statements before the paired code release exists: the Rust models still
-- read these columns as `NaiveDateTime`, which sqlx refuses to decode from TIMESTAMPTZ.
-- See docs/operations/retirement-tooling-section-16.md §DATA-001.

BEGIN READ ONLY;

\echo '== 1. time zones (values must have been written in UTC: sqlx sessions are UTC) =='
SELECT current_setting('TimeZone') AS session_time_zone,
       (SELECT reset_val FROM pg_settings WHERE name = 'TimeZone') AS server_default_time_zone;

\echo '== 2. naive timestamp columns, rewrite size and row estimate =='
SELECT c.table_name,
       c.column_name,
       c.is_nullable,
       pg_size_pretty(pg_total_relation_size(format('public.%I', c.table_name)::regclass)) AS table_total_size,
       pg_total_relation_size(format('public.%I', c.table_name)::regclass) AS table_total_bytes,
       (SELECT reltuples::bigint FROM pg_class WHERE oid = format('public.%I', c.table_name)::regclass) AS row_estimate
  FROM information_schema.columns c
 WHERE c.table_schema = 'public'
   AND c.data_type = 'timestamp without time zone'
   AND (c.table_name, c.column_name) NOT IN (
         ('meeting_recurrence_rules', 'dtstart_local'),
         ('meeting_recurrence_exceptions', 'original_start_local'))
   AND c.table_name NOT LIKE '%migrations'
 ORDER BY table_total_bytes DESC, c.table_name, c.ordinal_position;

\echo '== 3. defaults on those columns (CURRENT_TIMESTAMP into TIMESTAMP follows the session zone) =='
SELECT table_name, column_name, column_default
  FROM information_schema.columns
 WHERE table_schema = 'public'
   AND data_type = 'timestamp without time zone'
   AND column_default IS NOT NULL
 ORDER BY table_name, column_name;

\echo '== 4. plausibility: newest naive values must not be in the future (a non-UTC writer) =='
SELECT 'meetings.created_at' AS col, max(created_at) AS newest, now() AT TIME ZONE 'UTC' AS utc_now FROM meetings
UNION ALL SELECT 'users.updated_at', max(updated_at), now() AT TIME ZONE 'UTC' FROM users
UNION ALL SELECT 'session_logs.event_time', max(event_time), now() AT TIME ZONE 'UTC' FROM session_logs;

\echo '== 5. conversion statements for REVIEW ONLY (one ALTER per table = one rewrite) =='
SELECT format(
         'ALTER TABLE public.%I %s;',
         table_name,
         string_agg(format('ALTER COLUMN %I TYPE timestamptz USING %I AT TIME ZONE ''UTC''',
                           column_name, column_name), ', ' ORDER BY ordinal_position)) AS statement
  FROM information_schema.columns
 WHERE table_schema = 'public'
   AND data_type = 'timestamp without time zone'
   AND (table_name, column_name) NOT IN (
         ('meeting_recurrence_rules', 'dtstart_local'),
         ('meeting_recurrence_exceptions', 'original_start_local'))
   AND table_name NOT LIKE '%migrations'
 GROUP BY table_name
 ORDER BY table_name;

ROLLBACK;
