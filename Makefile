# CF Meet — root developer commands.
# Each target delegates to the app/service's own toolchain. services/meeting-service/,
# services/calling/, services/platform-access/, services/numbering/, services/privacy-relay/,
# services/telephony/, services/sms-gateway/, services/messaging/, services/developer-platform/,
# services/api-gateway/ and apps/web/ remain independently buildable
# and deployable.
# Shared code lives in packages/ (contracts, service-auth); no service depends on another.

MEETING_SERVICE := services/meeting-service
CALLING         := services/calling
PLATFORM_ACCESS := services/platform-access
NUMBERING       := services/numbering
PRIVACY_RELAY   := services/privacy-relay
TELEPHONY       := services/telephony
SMS_GATEWAY     := services/sms-gateway
MESSAGING       := services/messaging
MESSAGING_CRYPTO := packages/messaging-crypto
WEB             := apps/web
SERVICE_AUTH    := packages/service-auth
CONTRACTS       := packages/contracts
DEVELOPER_PLATFORM := services/developer-platform
API_GATEWAY     := services/api-gateway
CERTIFICATION   := certification

# Binaries of a service workspace (honours a shared CARGO_TARGET_DIR).
bin_dir = $(or $(CARGO_TARGET_DIR),$(CURDIR)/$(1)/target)/debug
# Calling and Numbering authenticate every credential through Platform Access: their tests
# run a real one.
PLATFORM_ACCESS_TEST_ENV = PLATFORM_ACCESS_BIN=$(call bin_dir,$(PLATFORM_ACCESS))/platform-access \
	PLATFORM_ACCESS_MIGRATION_BIN=$(call bin_dir,$(PLATFORM_ACCESS))/platform-access-migration

.PHONY: help deps deps-down meeting-service backend frontend web migrate dev test lint build \
        backend-test backend-lint backend-build frontend-test frontend-lint frontend-build \
        frontend-install calling calling-migrate calling-test calling-lint calling-build calling-admin \
        platform-access platform-access-migrate platform-access-test platform-access-lint \
        platform-access-build platform-access-admin platform-access-bins \
        numbering numbering-migrate numbering-test numbering-lint numbering-build \
        privacy-relay privacy-relay-migrate privacy-relay-test privacy-relay-lint privacy-relay-build \
        privacy-relay-integration \
        telephony telephony-migrate telephony-test telephony-lint telephony-build telephony-sip-cert \
        telephony-integration communications-e2e database-isolation \
        sms-gateway sms-gateway-migrate sms-gateway-test sms-gateway-lint sms-gateway-build \
        messaging messaging-migrate messaging-test messaging-lint messaging-build messaging-crypto-test \
        service-auth-test service-auth-lint contracts-validate contracts-test contracts-lint \
        service-boundaries secret-scan \
        developer-platform developer-platform-worker developer-platform-migrate developer-platform-test \
        developer-platform-lint developer-platform-build developer-platform-bins \
        api-gateway api-gateway-test api-gateway-lint api-gateway-build api-gateway-integration \
        sdks-check certification certification-bins certification-stack certification-lint

help: ## Show available commands
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-20s\033[0m %s\n", $$1, $$2}'

deps: ## Start local PostgreSQL, Redis and LiveKit (docker compose)
	docker compose up -d postgres redis livekit

deps-down: ## Stop local dependencies
	docker compose down

migrate: ## Apply meeting-service database migrations
	cd $(MEETING_SERVICE) && cargo run -p migration -- up

meeting-service: ## Run the Meeting & Conference Platform service
	cd $(MEETING_SERVICE) && cargo run --bin backend

backend: meeting-service ## Legacy alias for `make meeting-service`

calling-migrate: ## Apply Calling service database migrations (services/calling/.env)
	cd $(CALLING) && cargo run -p calling-migration -- up

calling: ## Run the Calling service
	cd $(CALLING) && cargo run --bin calling

calling-admin: ## Calling platform administration CLI, e.g. make calling-admin ARGS="tenant list"
	cd $(CALLING) && cargo run --bin platform_admin -- $(ARGS)

calling-test: platform-access-bins ## Run Calling tests against a real Platform Access (requires TEST_DATABASE_URL; no Docker needed)
	cd $(CALLING) && $(PLATFORM_ACCESS_TEST_ENV) cargo test --workspace --no-fail-fast

calling-lint: ## fmt + clippy for the Calling service
	cd $(CALLING) && cargo fmt --all -- --check
	cd $(CALLING) && cargo clippy --workspace --all-targets -- -D warnings

calling-build: ## Build the Calling service
	cd $(CALLING) && cargo build --workspace

platform-access-migrate: ## Apply Platform Access database migrations (services/platform-access/.env)
	cd $(PLATFORM_ACCESS) && cargo run -p platform-access-migration -- up

platform-access: ## Run the Platform Access identity authority
	cd $(PLATFORM_ACCESS) && cargo run --bin platform-access

platform-access-admin: ## Platform Access administration CLI, e.g. make platform-access-admin ARGS="tenant list"
	cd $(PLATFORM_ACCESS) && cargo run --bin platform-access-admin -- $(ARGS)

platform-access-bins: ## Build the Platform Access server + migration binaries (used by Calling's and Numbering's tests)
	cd $(PLATFORM_ACCESS) && cargo build --workspace --bins

platform-access-test: ## Run Platform Access tests (requires TEST_DATABASE_URL; no Docker needed)
	cd $(PLATFORM_ACCESS) && cargo test --workspace --no-fail-fast

platform-access-lint: ## fmt + clippy for Platform Access
	cd $(PLATFORM_ACCESS) && cargo fmt --all -- --check
	cd $(PLATFORM_ACCESS) && cargo clippy --workspace --all-targets -- -D warnings

platform-access-build: ## Build Platform Access
	cd $(PLATFORM_ACCESS) && cargo build --workspace

numbering-migrate: ## Apply Numbering database migrations (services/numbering/.env)
	cd $(NUMBERING) && cargo run -p numbering-migration -- up

numbering: ## Run the Numbering service (number resources, DCN assignments)
	cd $(NUMBERING) && cargo run --bin numbering

numbering-test: platform-access-bins ## Run Numbering tests against a real Platform Access (requires TEST_DATABASE_URL; no Docker needed)
	cd $(NUMBERING) && $(PLATFORM_ACCESS_TEST_ENV) cargo test --workspace --no-fail-fast

numbering-lint: ## fmt + clippy for Numbering
	cd $(NUMBERING) && cargo fmt --all -- --check
	cd $(NUMBERING) && cargo clippy --workspace --all-targets -- -D warnings

numbering-build: ## Build Numbering
	cd $(NUMBERING) && cargo build --workspace

privacy-relay-migrate: ## Apply Privacy Relay database migrations (services/privacy-relay/.env)
	cd $(PRIVACY_RELAY) && cargo run -p privacy-relay-migration -- up

privacy-relay: ## Run the Privacy Relay service
	cd $(PRIVACY_RELAY) && cargo run --bin privacy-relay

privacy-relay-test: ## Run Privacy Relay tests (requires TEST_DATABASE_URL; no Docker needed)
	cd $(PRIVACY_RELAY) && cargo test --workspace --no-fail-fast

privacy-relay-lint: ## fmt + clippy for the Privacy Relay service
	cd $(PRIVACY_RELAY) && cargo fmt --all -- --check
	cd $(PRIVACY_RELAY) && cargo clippy --workspace --all-targets -- -D warnings

privacy-relay-build: ## Build the Privacy Relay service
	cd $(PRIVACY_RELAY) && cargo build --workspace

privacy-relay-integration: platform-access-bins ## Calling <-> Privacy Relay e2e against the real binary (requires TEST_DATABASE_URL)
	cd $(PRIVACY_RELAY) && cargo build --workspace --bins
	cd $(CALLING) && $(PLATFORM_ACCESS_TEST_ENV) \
		PRIVACY_RELAY_BIN=$(call bin_dir,$(PRIVACY_RELAY))/privacy-relay \
		PRIVACY_RELAY_MIGRATION_BIN=$(call bin_dir,$(PRIVACY_RELAY))/privacy-relay-migration \
		cargo test --test privacy_relay_e2e

sms-gateway-migrate: ## Apply SMS Gateway database migrations (services/sms-gateway/.env)
	cd $(SMS_GATEWAY) && cargo run -p sms-gateway-migration -- up

sms-gateway: ## Run the SMS Gateway (SMS transport; deterministic provider for local use)
	cd $(SMS_GATEWAY) && cargo run --bin sms-gateway

sms-gateway-test: ## Run SMS Gateway tests (requires TEST_DATABASE_URL; no Docker needed)
	cd $(SMS_GATEWAY) && cargo test --workspace --no-fail-fast

sms-gateway-lint: ## fmt + clippy for the SMS Gateway
	cd $(SMS_GATEWAY) && cargo fmt --all -- --check
	cd $(SMS_GATEWAY) && cargo clippy --workspace --all-targets -- -D warnings

sms-gateway-build: ## Build the SMS Gateway
	cd $(SMS_GATEWAY) && cargo build --workspace

messaging-migrate: ## Apply Messaging database migrations (services/messaging/.env)
	cd $(MESSAGING) && cargo run -p messaging-migration -- up

messaging: ## Run the Messaging service (Platform Access required)
	cd $(MESSAGING) && cargo run --bin messaging

messaging-test: platform-access-bins ## Run Messaging tests (requires TEST_DATABASE_URL; the e2e runs the real Platform Access)
	cd $(MESSAGING) && $(PLATFORM_ACCESS_TEST_ENV) cargo test --workspace --no-fail-fast

messaging-lint: ## fmt + clippy for Messaging and its crypto core
	cd $(MESSAGING) && cargo fmt --all -- --check
	cd $(MESSAGING) && cargo clippy --workspace --all-targets -- -D warnings
	cd $(MESSAGING_CRYPTO) && cargo fmt --all -- --check
	cd $(MESSAGING_CRYPTO) && cargo clippy --all-targets --all-features -- -D warnings

messaging-build: ## Build Messaging
	cd $(MESSAGING) && cargo build --workspace

messaging-crypto-test: ## Test the Messaging crypto core (real OpenMLS objects)
	cd $(MESSAGING_CRYPTO) && cargo test --all-features

telephony-migrate: ## Apply Telephony database migrations (services/telephony/.env)
	cd $(TELEPHONY) && cargo run -p telephony-migration -- up

telephony: ## Run the Telephony service
	cd $(TELEPHONY) && cargo run --bin telephony

telephony-test: ## Run Telephony tests (requires TEST_DATABASE_URL; SIP certification runs if asterisk + sipp exist)
	cd $(TELEPHONY) && cargo test --workspace --no-fail-fast

telephony-lint: ## fmt + clippy for the Telephony service
	cd $(TELEPHONY) && cargo fmt --all -- --check
	cd $(TELEPHONY) && cargo clippy --workspace --all-targets -- -D warnings

telephony-build: ## Build the Telephony service
	cd $(TELEPHONY) && cargo build --workspace

telephony-sip-cert: ## Real SIP / media edge certification (requires asterisk + sipp, TEST_DATABASE_URL)
	cd $(TELEPHONY) && TELEPHONY_SIP_CERT_REQUIRED=1 cargo test --test sip_certification -- --nocapture

telephony-integration: platform-access-bins ## Calling <-> Telephony <-> Privacy Relay e2e against the real binaries (requires TEST_DATABASE_URL)
	cd $(PRIVACY_RELAY) && cargo build --workspace --bins
	cd $(TELEPHONY) && cargo build --workspace --bins
	cd $(CALLING) && $(PLATFORM_ACCESS_TEST_ENV) \
		PRIVACY_RELAY_BIN=$(call bin_dir,$(PRIVACY_RELAY))/privacy-relay \
		PRIVACY_RELAY_MIGRATION_BIN=$(call bin_dir,$(PRIVACY_RELAY))/privacy-relay-migration \
		TELEPHONY_BIN=$(call bin_dir,$(TELEPHONY))/telephony \
		TELEPHONY_MIGRATION_BIN=$(call bin_dir,$(TELEPHONY))/telephony-migration \
		cargo test --test telephony_e2e

# Part IV Step 20: SMS and voice journeys across real processes of Platform Access, Numbering,
# the SMS Gateway, Calling, Telephony and Privacy Relay. The voice journeys also need
# asterisk + sipp (the certification SIP edge), redis-server, LIVEKIT_SERVER_BIN (Calling's
# media), LIVEKIT_SIP_BIN (its SIP bridge) and CALLING_RTC_CERT_PYTHON (a Python with the
# LiveKit RTC SDK: the application's media client, run from Calling's RTC certification
# client, CALLING_RTC_CLIENT, set below); without them they are SKIPPED.
communications-e2e: platform-access-bins ## Cross-service SMS + voice journeys and the Part P relay-number cutover (requires TEST_DATABASE_URL; voice: asterisk, sipp, redis-server, LIVEKIT_SERVER_BIN, LIVEKIT_SIP_BIN, CALLING_RTC_CERT_PYTHON)
	cd $(SMS_GATEWAY) && cargo build --workspace --bins
	cd $(CALLING) && cargo build --workspace --bins
	cd $(PRIVACY_RELAY) && cargo build --workspace --bins
	cd $(TELEPHONY) && cargo build --workspace --bins
	cd $(NUMBERING) && $(PLATFORM_ACCESS_TEST_ENV) CFMEET_REQUIRE_E2E=1 \
		SMS_GATEWAY_BIN=$(call bin_dir,$(SMS_GATEWAY))/sms-gateway \
		SMS_GATEWAY_MIGRATION_BIN=$(call bin_dir,$(SMS_GATEWAY))/sms-gateway-migration \
		CALLING_BIN=$(call bin_dir,$(CALLING))/calling \
		CALLING_MIGRATION_BIN=$(call bin_dir,$(CALLING))/calling-migration \
		PRIVACY_RELAY_BIN=$(call bin_dir,$(PRIVACY_RELAY))/privacy-relay \
		PRIVACY_RELAY_MIGRATION_BIN=$(call bin_dir,$(PRIVACY_RELAY))/privacy-relay-migration \
		TELEPHONY_BIN=$(call bin_dir,$(TELEPHONY))/telephony \
		TELEPHONY_MIGRATION_BIN=$(call bin_dir,$(TELEPHONY))/telephony-migration \
		CALLING_RTC_CLIENT=$(CURDIR)/$(CALLING)/tests/rtc/rtc_client.py \
		cargo test --test communications_e2e --test relay_cutover_e2e -- --nocapture

# Part VI (§§1–5 foundation): the Developer Platform (own database) and the public API
# Gateway (no database). Both authenticate through Platform Access only.
developer-platform-migrate: ## Apply Developer Platform migrations (services/developer-platform/.env)
	cd $(DEVELOPER_PLATFORM) && cargo run -p developer-platform-migration -- up

developer-platform: ## Run the Developer Platform API (Platform Access required)
	cd $(DEVELOPER_PLATFORM) && cargo run --bin developer-platform

developer-platform-worker: ## Run the Developer Platform worker
	cd $(DEVELOPER_PLATFORM) && cargo run --bin developer-platform-worker

developer-platform-test: ## Run Developer Platform tests (requires TEST_DATABASE_URL; no Docker needed)
	cd $(DEVELOPER_PLATFORM) && cargo test --workspace --no-fail-fast

developer-platform-lint: ## fmt + clippy for the Developer Platform
	cd $(DEVELOPER_PLATFORM) && cargo fmt --all -- --check
	cd $(DEVELOPER_PLATFORM) && cargo clippy --workspace --all-targets -- -D warnings

developer-platform-build: ## Build the Developer Platform
	cd $(DEVELOPER_PLATFORM) && cargo build --workspace

developer-platform-bins: ## Build the Developer Platform server + migration binaries (used by the gateway journey)
	cd $(DEVELOPER_PLATFORM) && cargo build --workspace --bins

api-gateway: ## Run the API Gateway (default deny; Platform Access required)
	cd $(API_GATEWAY) && cargo run --bin api-gateway

api-gateway-test: ## API Gateway unit / ingress / routing / startup tests (no database, no Docker)
	cd $(API_GATEWAY) && CFMEET_SKIP_DB_TESTS=1 cargo test --no-fail-fast

api-gateway-integration: platform-access-bins developer-platform-bins ## Gateway <-> Platform Access <-> Developer Platform journey against the real binaries (requires TEST_DATABASE_URL)
	cd $(API_GATEWAY) && $(PLATFORM_ACCESS_TEST_ENV) \
		DEVELOPER_PLATFORM_BIN=$(call bin_dir,$(DEVELOPER_PLATFORM))/developer-platform \
		DEVELOPER_PLATFORM_MIGRATION_BIN=$(call bin_dir,$(DEVELOPER_PLATFORM))/developer-platform-migration \
		cargo test --test platform_access_e2e -- --nocapture

api-gateway-lint: ## fmt + clippy for the API Gateway
	cd $(API_GATEWAY) && cargo fmt --all -- --check
	cd $(API_GATEWAY) && cargo clippy --all-targets -- -D warnings

api-gateway-build: ## Build the API Gateway
	cd $(API_GATEWAY) && cargo build

sdks-check: ## SDK workspace scaffolding check (TypeScript + Dart; generation is Section 52)
	python3 sdks/generate.py --check

# Part VI cross-service certification (certification/, a stand-alone non-deployable test
# workspace): the REAL service binaries, built into ONE shared target directory.
CERT_TARGET_DIR := $(or $(CARGO_TARGET_DIR),$(CURDIR)/target)
CERTIFICATION_SERVICES := $(PLATFORM_ACCESS) $(DEVELOPER_PLATFORM) $(API_GATEWAY) $(MEETING_SERVICE) \
	$(CALLING) $(MESSAGING) $(NUMBERING) $(SMS_GATEWAY) $(PRIVACY_RELAY)
CERT_ENV = CARGO_TARGET_DIR=$(CERT_TARGET_DIR) CFMEET_BIN_DIR=$(CERT_TARGET_DIR)/debug

certification-bins: ## Build every service binary the certification stack runs (one shared CARGO_TARGET_DIR)
	@set -e; for s in $(CERTIFICATION_SERVICES); do \
		echo "==> $$s"; (cd $$s && CARGO_TARGET_DIR=$(CERT_TARGET_DIR) cargo build --workspace --bins); \
	done

certification: certification-bins ## Part VI certification journeys against the real processes (requires TEST_DATABASE_URL, TEST_REDIS_URL)
	cd $(CERTIFICATION) && $(CERT_ENV) cargo test --locked --no-fail-fast -- --test-threads=1

certification-stack: certification-bins ## Run the certification stack for SDK integration tests (ENV_OUT=<path>, default certification/stack-env.json)
	cd $(CERTIFICATION) && $(CERT_ENV) cargo run --locked --bin cfmeet-cert-stack -- up \
		--env-out $(or $(ENV_OUT),$(CURDIR)/$(CERTIFICATION)/stack-env.json)

certification-lint: ## fmt + clippy for the certification harness
	cd $(CERTIFICATION) && cargo fmt --check
	cd $(CERTIFICATION) && cargo clippy --locked --all-targets -- -D warnings

service-auth-test: ## Test packages/service-auth (core and client feature sets)
	cd $(SERVICE_AUTH) && cargo test --no-fail-fast
	cd $(SERVICE_AUTH) && cargo test --no-fail-fast --features client

service-auth-lint: ## fmt + clippy for packages/service-auth
	cd $(SERVICE_AUTH) && cargo fmt --all -- --check
	cd $(SERVICE_AUTH) && cargo clippy --all-targets -- -D warnings
	cd $(SERVICE_AUTH) && cargo clippy --all-targets --features client -- -D warnings

contracts-validate: ## Validate packages/contracts (manifest, schemas, OpenAPI, fixtures; offline, no Docker)
	cd $(CONTRACTS)/validator && cargo run -q -- ../ ../../..

contracts-test: ## Contract validator mutation tests
	cd $(CONTRACTS)/validator && cargo test --no-fail-fast

contracts-lint: ## fmt + clippy for the contract validator
	cd $(CONTRACTS)/validator && cargo fmt --all -- --check
	cd $(CONTRACTS)/validator && cargo clippy --all-targets -- -D warnings

service-boundaries: ## Repository service-boundary guard, operations registry + their mutation tests (Python stdlib)
	python3 -m unittest discover -s scripts/service-boundaries
	python3 scripts/service-boundaries/check_service_boundaries.py
	python3 scripts/service-boundaries/check_operations_registry.py

database-isolation: ## Per-service database roles: least privilege and no cross-service connect (ADMIN_URL=postgres://... superuser)
	infra/deployment/postgres/verify-service-isolation.sh --admin-url "$(ADMIN_URL)"

secret-scan: ## Repository secret scan (keys, keyrings, credentials) + its mutation tests; `--tree DIR` scans an exported image
	python3 -m unittest discover -s scripts/secret-scan
	python3 scripts/secret-scan/scan_secrets.py

frontend: frontend-install ## Run the web app dev server (apps/web)
	cd $(WEB) && npm run dev

web: frontend ## Alias for `make frontend`

dev: ## Start deps, then meeting service + web app together (Ctrl+C stops both)
	./scripts/dev.sh

test: backend-test calling-test platform-access-test numbering-test privacy-relay-test telephony-test sms-gateway-test developer-platform-test api-gateway-test service-auth-test contracts-test contracts-validate service-boundaries secret-scan frontend-test ## Run all tests

lint: backend-lint calling-lint platform-access-lint numbering-lint privacy-relay-lint telephony-lint sms-gateway-lint developer-platform-lint api-gateway-lint service-auth-lint contracts-lint frontend-lint ## Run all linters

build: backend-build calling-build platform-access-build numbering-build privacy-relay-build telephony-build sms-gateway-build developer-platform-build api-gateway-build frontend-build ## Build all applications and services

backend-test:
	cd $(MEETING_SERVICE) && cargo test --workspace

backend-lint:
	cd $(MEETING_SERVICE) && cargo fmt --all -- --check
	cd $(MEETING_SERVICE) && cargo clippy --workspace --all-targets

backend-build:
	cd $(MEETING_SERVICE) && cargo build --workspace

frontend-install:
	@test -d $(WEB)/node_modules || (cd $(WEB) && npm ci)

frontend-test: frontend-install
	@echo "apps/web: no test suite configured; running typecheck"
	cd $(WEB) && npx tsc --noEmit

frontend-lint: frontend-install
	cd $(WEB) && npm run lint

frontend-build: frontend-install
	cd $(WEB) && npm run build
