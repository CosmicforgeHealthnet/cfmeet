# CF Meet — root developer commands.
# Each target delegates to the app/service's own toolchain. services/meeting-service/
# and apps/web/ remain independently buildable and deployable.

MEETING_SERVICE := services/meeting-service
WEB             := apps/web

.PHONY: help deps deps-down meeting-service backend frontend web migrate dev test lint build \
        backend-test backend-lint backend-build frontend-test frontend-lint frontend-build \
        frontend-install

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

frontend: frontend-install ## Run the web app dev server (apps/web)
	cd $(WEB) && npm run dev

web: frontend ## Alias for `make frontend`

dev: ## Start deps, then meeting service + web app together (Ctrl+C stops both)
	./scripts/dev.sh

test: backend-test frontend-test ## Run all tests

lint: backend-lint frontend-lint ## Run all linters

build: backend-build frontend-build ## Build both applications

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
