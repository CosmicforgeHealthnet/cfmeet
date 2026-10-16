# CF Meet — root developer commands.
# Each target delegates to the app/service's own toolchain. services/meeting-service/,
# services/calling/, services/platform-access/, services/privacy-relay/, services/telephony/ and
# apps/web/ remain independently buildable and deployable.
# Shared code lives in packages/ (contracts, service-auth); no service depends on another.

MEETING_SERVICE := services/meeting-service
CALLING         := services/calling
PLATFORM_ACCESS := services/platform-access
PRIVACY_RELAY   := services/privacy-relay
TELEPHONY       := services/telephony
WEB             := apps/web
SERVICE_AUTH    := packages/service-auth
CONTRACTS       := packages/contracts

# Binaries of a service workspace (honours a shared CARGO_TARGET_DIR).
bin_dir = $(or $(CARGO_TARGET_DIR),$(CURDIR)/$(1)/target)/debug
# Calling authenticates every credential through Platform Access: its tests run a real one.
PLATFORM_ACCESS_TEST_ENV = PLATFORM_ACCESS_BIN=$(call bin_dir,$(PLATFORM_ACCESS))/platform-access \
	PLATFORM_ACCESS_MIGRATION_BIN=$(call bin_dir,$(PLATFORM_ACCESS))/platform-access-migration

.PHONY: help deps deps-down meeting-service backend frontend web migrate dev test lint build \
        backend-test backend-lint backend-build frontend-test frontend-lint frontend-build \
        frontend-install calling calling-migrate calling-test calling-lint calling-build calling-admin \
        platform-access platform-access-migrate platform-access-test platform-access-lint \
        platform-access-build platform-access-admin platform-access-bins \
        privacy-relay privacy-relay-migrate privacy-relay-test privacy-relay-lint privacy-relay-build \
        privacy-relay-integration \
        telephony telephony-migrate telephony-test telephony-lint telephony-build telephony-sip-cert \
        telephony-integration database-isolation \
        service-auth-test service-auth-lint contracts-validate contracts-test contracts-lint \
        service-boundaries secret-scan

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

platform-access-bins: ## Build the Platform Access server + migration binaries (used by Calling's tests)
	cd $(PLATFORM_ACCESS) && cargo build --workspace --bins

platform-access-test: ## Run Platform Access tests (requires TEST_DATABASE_URL; no Docker needed)
	cd $(PLATFORM_ACCESS) && cargo test --workspace --no-fail-fast

platform-access-lint: ## fmt + clippy for Platform Access
	cd $(PLATFORM_ACCESS) && cargo fmt --all -- --check
	cd $(PLATFORM_ACCESS) && cargo clippy --workspace --all-targets -- -D warnings

platform-access-build: ## Build Platform Access
	cd $(PLATFORM_ACCESS) && cargo build --workspace

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

test: backend-test calling-test platform-access-test privacy-relay-test telephony-test service-auth-test contracts-test contracts-validate service-boundaries secret-scan frontend-test ## Run all tests

lint: backend-lint calling-lint platform-access-lint privacy-relay-lint telephony-lint service-auth-lint contracts-lint frontend-lint ## Run all linters

build: backend-build calling-build platform-access-build privacy-relay-build telephony-build frontend-build ## Build all applications and services

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
