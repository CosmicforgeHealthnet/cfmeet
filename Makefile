# CF Meet — root developer commands.
# Each target delegates to the app's own toolchain; backend/ and frontend/
# remain independently buildable and deployable.

.PHONY: help deps deps-down backend frontend migrate dev test lint build \
        backend-test backend-lint backend-build frontend-test frontend-lint frontend-build \
        frontend-install

help: ## Show available commands
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2}'

deps: ## Start local PostgreSQL, Redis and LiveKit (docker compose)
	docker compose up -d postgres redis livekit

deps-down: ## Stop local dependencies
	docker compose down

migrate: ## Apply backend database migrations
	cd backend && cargo run -p migration -- up

backend: ## Run the backend API (cargo run)
	cd backend && cargo run --bin backend

frontend: frontend-install ## Run the frontend dev server
	cd frontend && npm run dev

dev: ## Start deps, then backend + frontend together (Ctrl+C stops both)
	./scripts/dev.sh

test: backend-test frontend-test ## Run all tests

lint: backend-lint frontend-lint ## Run all linters

build: backend-build frontend-build ## Build both applications

backend-test:
	cd backend && cargo test --workspace

backend-lint:
	cd backend && cargo fmt --all -- --check
	cd backend && cargo clippy --workspace --all-targets

backend-build:
	cd backend && cargo build --workspace

frontend-install:
	@test -d frontend/node_modules || (cd frontend && npm ci)

frontend-test: frontend-install
	@echo "frontend: no test suite configured; running typecheck"
	cd frontend && npx tsc --noEmit

frontend-lint: frontend-install
	cd frontend && npm run lint

frontend-build: frontend-install
	cd frontend && npm run build
