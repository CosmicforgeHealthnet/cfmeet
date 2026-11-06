# shellcheck shell=bash
# Shared helpers for the CF Meet database backup tooling. Sourced by the scripts in this
# directory, never executed on its own. Callers set CFB_PROG before sourcing.
#
# Secrets: a database URL is converted into libpq environment variables (PGHOST, PGPORT,
# PGUSER, PGPASSWORD, PGDATABASE, PGSSLMODE, ...) by cfb_use_url and is never placed on a
# child process's command line, written to a file or printed. Error messages name the
# setting that is wrong, never its value.

CFB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CFB_CATALOG="${CFMEET_BACKUP_CATALOG:-$CFB_DIR/catalog.conf}"
CFB_PROG="${CFB_PROG:-backup}"
# shellcheck disable=SC2034 # used by the scripts that source this file
CFB_LOCAL_ONLY_NOTE="LOCAL_ONLY — not disaster recovery"

cfb_log() { printf '%s: %s\n' "$CFB_PROG" "$*" >&2; }
cfb_die() {
  local code="$1"
  shift
  cfb_log "$*"
  exit "$code"
}

# ---------------------------------------------------------------- catalog

# cfb_catalog_services [status]: service names in catalog order (all, or one status).
cfb_catalog_services() {
  awk -v want="${1:-}" '$1 !~ /^#/ && NF == 5 && (want == "" || $2 == want) { print $1 }' "$CFB_CATALOG"
}

# cfb_load_service <service>: sets CFB_STATUS and the arrays CFB_REQUIRED, CFB_OPTIONAL,
# CFB_IDENTITY. Returns 1 for an unknown service or a malformed line.
cfb_load_service() {
  local service="$1" line status req opt ident name
  [[ "$service" =~ ^[a-z][a-z0-9-]{1,40}$ ]] || return 1
  [ -r "$CFB_CATALOG" ] || return 1
  line="$(awk -v s="$service" '$1 !~ /^#/ && NF == 5 && $1 == s { print $2, $3, $4, $5; exit }' "$CFB_CATALOG")"
  [ -n "$line" ] || return 1
  read -r status req opt ident <<<"$line"
  CFB_STATUS="$status"
  CFB_REQUIRED=()
  CFB_OPTIONAL=()
  CFB_IDENTITY=()
  [ "$req" = "-" ] || IFS=, read -r -a CFB_REQUIRED <<<"$req"
  [ "$opt" = "-" ] || IFS=, read -r -a CFB_OPTIONAL <<<"$opt"
  [ "$ident" = "-" ] || IFS=, read -r -a CFB_IDENTITY <<<"$ident"
  [ "${#CFB_REQUIRED[@]}" -gt 0 ] || return 1
  # Table names are interpolated into SQL: accept plain lower-case identifiers only.
  for name in "${CFB_REQUIRED[@]}" "${CFB_OPTIONAL[@]}" "${CFB_IDENTITY[@]}"; do
    [[ "$name" =~ ^[a-z_][a-z0-9_]{0,62}$ ]] || return 1
  done
  case "$status" in active | reserved) ;; *) return 1 ;; esac
}

# cfb_require_active_service <service>: loads the service or exits 2.
cfb_require_active_service() {
  local service="$1"
  cfb_load_service "$service" ||
    cfb_die 2 "--service must be one of: $(cfb_catalog_services active | tr '\n' ' ')(see catalog.conf)"
  [ "$CFB_STATUS" = active ] ||
    cfb_die 2 "service $service is a reserved catalog slot; activate it in catalog.conf when its migrations ship"
}

# ---------------------------------------------------------------- connection

# cfb_read_url_source <url> <url-file> <env-file> <env-var>: prints the database URL from
# the first configured source (a file holding only the URL, a docker-style env file holding
# <env-var>=<url>, the literal URL, then the DATABASE_URL environment variable). Prints
# nothing when no source is configured. Never logs the URL.
cfb_read_url_source() {
  local url="$1" url_file="$2" env_file="$3" var="${4:-DATABASE_URL}" value=""
  if [ -n "$url_file" ]; then
    [ -r "$url_file" ] || cfb_die 2 "database URL file is not readable: $url_file"
    value="$(head -n 1 "$url_file" | tr -d '\r')"
    [ -n "$value" ] || cfb_die 2 "database URL file is empty: $url_file"
  elif [ -n "$env_file" ]; then
    [ -r "$env_file" ] || cfb_die 2 "env file is not readable: $env_file"
    [[ "$var" =~ ^[A-Z_][A-Z0-9_]*$ ]] || cfb_die 2 "invalid env variable name"
    value="$(awk -v k="$var=" 'index($0, k) == 1 { print substr($0, length(k) + 1); exit }' "$env_file" | tr -d '\r')"
    value="${value#\"}"
    value="${value%\"}"
    [ -n "$value" ] || cfb_die 2 "$var is not set in $env_file"
  elif [ -n "$url" ]; then
    value="$url"
  else
    value="${DATABASE_URL:-}"
  fi
  printf '%s' "$value"
}

# cfb_use_url <url>: exports the libpq variables for <url>. The URL travels to python3 on
# stdin (never argv). Unsupported query parameters are refused by name.
cfb_use_url() {
  local assignments
  assignments="$(python3 -c '
import shlex, sys, urllib.parse as u
try:
    raw = sys.stdin.read().strip()
    p = u.urlsplit(raw)
    if p.scheme not in ("postgres", "postgresql"):
        sys.exit("database URL must start with postgres:// or postgresql://")
    env = {}
    if p.hostname:
        env["PGHOST"] = u.unquote(p.hostname)
    if p.port:
        env["PGPORT"] = str(p.port)
    if p.username:
        env["PGUSER"] = u.unquote(p.username)
    if p.password is not None:
        env["PGPASSWORD"] = u.unquote(p.password)
    db = u.unquote(p.path.lstrip("/"))
    if db:
        env["PGDATABASE"] = db
    known = {"sslmode": "PGSSLMODE", "sslrootcert": "PGSSLROOTCERT", "sslcert": "PGSSLCERT",
             "sslkey": "PGSSLKEY", "connect_timeout": "PGCONNECT_TIMEOUT",
             "application_name": "PGAPPNAME", "options": "PGOPTIONS",
             "target_session_attrs": "PGTARGETSESSIONATTRS", "passfile": "PGPASSFILE",
             "host": "PGHOST", "port": "PGPORT", "user": "PGUSER", "dbname": "PGDATABASE"}
    for k, v in u.parse_qsl(p.query, keep_blank_values=True):
        if k not in known:
            sys.exit("unsupported database URL parameter: " + k)
        env[known[k]] = v
    if "PGDATABASE" not in env:
        sys.exit("database URL names no database")
    for k, v in env.items():
        if "\n" in v or "\0" in v:
            sys.exit("database URL contains a control character")
        print("export %s=%s" % (k, shlex.quote(v)))
except SystemExit:
    raise
except Exception:
    sys.exit("database URL could not be parsed")
' <<<"$1")" || return 1
  eval "$assignments"
}

# cfb_connect <url> <url-file> <env-file> [env-var]: resolve and apply a connection.
# Falls back to an already prepared libpq environment (PGDATABASE set) when no URL source
# is configured. Exits 2 when nothing usable is configured.
cfb_connect() {
  local value
  value="$(cfb_read_url_source "$1" "$2" "$3" "${4:-DATABASE_URL}")" || exit "$?"
  if [ -n "$value" ]; then
    cfb_use_url "$value" || cfb_die 2 "the database URL was refused (see the message above)"
  elif [ -z "${PGDATABASE:-}" ]; then
    cfb_die 2 "no database connection: pass --database-url-file, --database-url-env-file or --database-url, or set DATABASE_URL"
  fi
}

# cfb_pg <tool> [args...]: run a PostgreSQL client tool directly, or through the command
# prefix in CFMEET_PG_WRAPPER (e.g. a `docker run --rm -i --network <net> -e PGHOST ...
# postgres:16-alpine` client container for a database that publishes no host port).
cfb_pg() {
  local -a wrapper=()
  if [ -n "${CFMEET_PG_WRAPPER:-}" ]; then
    read -r -a wrapper <<<"$CFMEET_PG_WRAPPER"
  fi
  "${wrapper[@]}" "$@"
}

# cfb_require_pg_tools <tool...>: the client tools must be on PATH unless a wrapper runs them.
cfb_require_pg_tools() {
  local tool
  if [ -n "${CFMEET_PG_WRAPPER:-}" ]; then
    return 0
  fi
  for tool in "$@"; do
    command -v "$tool" >/dev/null || cfb_die 3 "$tool not found"
  done
}

cfb_require_tools() {
  local tool
  for tool in "$@"; do
    command -v "$tool" >/dev/null || cfb_die 3 "$tool not found"
  done
}

# cfb_sql <query>: one query against the current libpq database, unaligned tuples only.
cfb_sql() { cfb_pg psql -XAtq -v ON_ERROR_STOP=1 -c "$1"; }

cfb_table_exists() {
  [ "$(cfb_sql "SELECT count(*) FROM pg_tables WHERE schemaname = 'public' AND tablename = '$1'")" = 1 ]
}

# cfb_history_snapshot: one line per catalog history table of the loaded service:
#   name|required(true/false)|present(true/false)|rows|max_version
cfb_history_snapshot() {
  local name required rows max r
  for name in "${CFB_REQUIRED[@]}" "${CFB_OPTIONAL[@]}"; do
    required=false
    for r in "${CFB_REQUIRED[@]}"; do
      if [ "$r" = "$name" ]; then required=true; fi
    done
    if cfb_table_exists "$name"; then
      rows="$(cfb_sql "SELECT count(*) FROM public.$name")"
      max="$(cfb_sql "SELECT coalesce(max(version)::text, '') FROM public.$name")"
      printf '%s|%s|true|%s|%s\n' "$name" "$required" "$rows" "$max"
    else
      printf '%s|%s|false|0|\n' "$name" "$required"
    fi
  done
}

cfb_public_table_count() { cfb_sql "SELECT count(*) FROM pg_tables WHERE schemaname = 'public'"; }

# cfb_pg_major_of <version text>: "pg_dump (PostgreSQL) 16.4 (Debian ...)" -> 16
cfb_pg_major_of() { printf '%s' "$1" | grep -oE '[0-9]+(\.[0-9]+)?' | head -n 1 | cut -d. -f1; }

# ---------------------------------------------------------------- integrity

cfb_sha256() { sha256sum "$1" | awk '{ print $1 }'; }
cfb_size() { wc -c <"$1" | tr -d ' '; }

# cfb_write_sha256 <file>: <file>.sha256 in sha256sum format, relative to its directory.
cfb_write_sha256() {
  local dir base
  dir="$(dirname "$1")"
  base="$(basename "$1")"
  (cd "$dir" && sha256sum "$base" >"$base.sha256")
}

# cfb_check_sha256 <file>: the file matches the first field of <file>.sha256.
cfb_check_sha256() {
  local expected actual
  [ -f "$1" ] && [ -f "$1.sha256" ] || return 2
  expected="$(awk 'NR == 1 { print $1 }' "$1.sha256")"
  actual="$(cfb_sha256 "$1")"
  [[ "$expected" =~ ^[0-9a-f]{64}$ ]] && [ "$expected" = "$actual" ]
}

# ---------------------------------------------------------------- encryption (age)

# cfb_check_recipients <file>: a readable age recipients file with at least one PUBLIC
# recipient and no private key material. Exits 5 otherwise: a private key never belongs on
# the database host.
cfb_check_recipients() {
  local file="$1"
  [ -n "$file" ] || cfb_die 5 "no age recipients file configured; refusing to write an unencrypted scheduled backup"
  [ -r "$file" ] && [ -s "$file" ] || cfb_die 5 "age recipients file is missing or empty: $file"
  if grep -q 'AGE-SECRET-KEY-' "$file" || grep -q 'PRIVATE KEY' "$file"; then
    cfb_die 5 "the recipients file contains PRIVATE key material; only public recipients (age1..., ssh-ed25519 ...) belong on this host"
  fi
  grep -qE '^(age1[0-9a-z]{20,}|ssh-ed25519 [A-Za-z0-9+/=]+|ssh-rsa [A-Za-z0-9+/=]+)' "$file" ||
    cfb_die 5 "age recipients file holds no age1... / ssh-ed25519 / ssh-rsa public recipient: $file"
}

# cfb_recipients_list <file>: the public recipients, one per line (comments stripped).
cfb_recipients_list() {
  grep -E '^(age1|ssh-ed25519 |ssh-rsa )' "$1" | awk '{ if ($1 ~ /^age1/) print $1; else print $1 " " $2 }'
}

# cfb_encrypt <recipients-file> <in> <out>: age-encrypt, write atomically, check the header.
cfb_encrypt() {
  local recipients="$1" in="$2" out="$3"
  age --encrypt --recipients-file "$recipients" --output "$out.partial" "$in" || {
    rm -f "$out.partial"
    return 1
  }
  [ "$(head -c 21 "$out.partial")" = "age-encryption.org/v1" ] || {
    rm -f "$out.partial"
    return 1
  }
  mv "$out.partial" "$out"
}

# ---------------------------------------------------------------- configuration

# cfb_load_config <file>: export CFMEET_* KEY=VALUE lines from an env file without executing
# it. Refuses a group/world-writable file (it decides what is dumped and where it is sent).
cfb_load_config() {
  local file="$1" line key value mode
  [ -r "$file" ] || cfb_die 2 "config file is not readable: $file"
  mode="$(stat -c '%a' "$file" 2>/dev/null || echo 600)"
  case "$mode" in *[2367]? | *[2367]) cfb_die 2 "config file $file is group/world-writable (mode $mode)" ;; esac
  while IFS= read -r line || [ -n "$line" ]; do
    line="${line%$'\r'}"
    case "$line" in '' | '#'*) continue ;; esac
    [[ "$line" =~ ^(CFMEET_[A-Z0-9_]+)=(.*)$ ]] || cfb_die 2 "config file $file: unsupported line (only CFMEET_*=value lines are allowed)"
    key="${BASH_REMATCH[1]}"
    value="${BASH_REMATCH[2]}"
    if [[ "$value" =~ ^\"(.*)\"$ ]] || [[ "$value" =~ ^\'(.*)\'$ ]]; then value="${BASH_REMATCH[1]}"; fi
    export "$key=$value"
  done <"$file"
}

# cfb_service_var <service> <suffix>: value of CFMEET_BACKUP_<SERVICE>_<SUFFIX> (may be empty).
cfb_service_var() {
  local name
  name="CFMEET_BACKUP_$(printf '%s' "$1" | tr 'a-z-' 'A-Z_')_$2"
  printf '%s' "${!name:-}"
}

cfb_utc_stamp() { date -u +%Y%m%dT%H%M%SZ; }
