.PHONY: build build-release run run-release check fmt fmt-check lint test test-unit openapi clean help

# --- Build ---

build: ## Build debug binary
	cargo build -p bridge

build-release: ## Build optimized release binary
	cargo build --release -p bridge

# --- Run ---

run: ## Run bridge (debug)
	cargo run -p bridge

run-release: ## Run bridge (release)
	cargo run --release -p bridge

# --- Check / Lint / Format ---

check: ## Type-check all crates
	cargo check --workspace

fmt: ## Format all code
	cargo fmt --all

fmt-check: ## Check formatting without modifying
	cargo fmt --all -- --check

lint: ## Run clippy linter
	cargo clippy --workspace -- -D warnings

# --- Tests ---

test: ## Run all unit + integration tests
	cargo test --workspace

test-unit: ## Run library tests only
	cargo test --workspace --lib

# --- OpenAPI ---

openapi: ## Generate OpenAPI v3 spec (openapi.json)
	cargo run -p bridge --features openapi --bin gen-openapi

# --- Clean ---

clean: ## Remove build artifacts
	cargo clean

# --- Help ---

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-20s\033[0m %s\n", $$1, $$2}'
