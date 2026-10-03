# CF Meet — root developer commands.
# Each target delegates to the app's own toolchain. services/core-api/ and
# apps/web/ remain independently buildable and deployable.

CORE_API := services/core-api
WEB      := apps/web

.PHONY: help deps deps-down backend frontend core-api web migrate dev test lint build \
        backend-test backend-lint backend-build frontend-test frontend-lint frontend-build \
        frontend-install

help: ## Show available commands
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2}'

deps: ## Start local PostgreSQL, Redis and LiveKit (docker compose)
	docker compose up -d postgres redis livekit

deps-down: ## Stop local dependencies
	docker compose down

migrate: ## Apply core-api database migrations
	cd $(CORE_API) && cargo run -p migration -- up

backend: ## Run the core API (services/core-api)
	cd $(CORE_API) && cargo run --bin backend

core-api: backend ## Alias for `make backend`

frontend: frontend-install ## Run the web app dev server (apps/web)
	cd $(WEB) && npm run dev

web: frontend ## Alias for `make frontend`

dev: ## Start deps, then core API + web app together (Ctrl+C stops both)
	./scripts/dev.sh

test: backend-test frontend-test ## Run all tests

lint: backend-lint frontend-lint ## Run all linters

build: backend-build frontend-build ## Build both applications

backend-test:
	cd $(CORE_API) && cargo test --workspace

backend-lint:
	cd $(CORE_API) && cargo fmt --all -- --check
	cd $(CORE_API) && cargo clippy --workspace --all-targets

backend-build:
	cd $(CORE_API) && cargo build --workspace

frontend-install:
	@test -d $(WEB)/node_modules || (cd $(WEB) && npm ci)

frontend-test: frontend-install
	@echo "apps/web: no test suite configured; running typecheck"
	cd $(WEB) && npx tsc --noEmit

frontend-lint: frontend-install
	cd $(WEB) && npm run lint

frontend-build: frontend-install
	cd $(WEB) && npm run build
