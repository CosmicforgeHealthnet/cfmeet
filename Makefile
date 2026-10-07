# CF Meet — root developer commands.
# Each target delegates to the app/service's own toolchain. services/meeting-service/,
# services/calling/ and apps/web/ remain independently buildable and deployable.

MEETING_SERVICE := services/meeting-service
CALLING         := services/calling
WEB             := apps/web

.PHONY: help deps deps-down meeting-service backend frontend web migrate dev test lint build \
        backend-test backend-lint backend-build frontend-test frontend-lint frontend-build \
        frontend-install calling calling-migrate calling-test calling-lint calling-build calling-admin

help: ## Show available commands
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2}'

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

calling-test: ## Run Calling tests (requires TEST_DATABASE_URL; no Docker needed)
	cd $(CALLING) && cargo test --workspace --no-fail-fast

calling-lint: ## fmt + clippy for the Calling service
	cd $(CALLING) && cargo fmt --all -- --check
	cd $(CALLING) && cargo clippy --workspace --all-targets -- -D warnings

calling-build: ## Build the Calling service
	cd $(CALLING) && cargo build --workspace

frontend: frontend-install ## Run the web app dev server (apps/web)
	cd $(WEB) && npm run dev

web: frontend ## Alias for `make frontend`

dev: ## Start deps, then meeting service + web app together (Ctrl+C stops both)
	./scripts/dev.sh

test: backend-test calling-test frontend-test ## Run all tests

lint: backend-lint calling-lint frontend-lint ## Run all linters

build: backend-build calling-build frontend-build ## Build all applications and services

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
