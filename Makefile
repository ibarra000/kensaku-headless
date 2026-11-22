# Kensaku — common development tasks.
#
#   make            # this help
#   make build      # release build of the `kensaku` binary
#   make test       # Rust test suite
#   make lint       # clippy, warnings as errors
#   make smoke      # end-to-end headless smoke test
#   make bench      # filename-search benchmark (needs a directory)
#   make clean      # remove build outputs (not your data)
#
# Data management lives in the binary: `kensaku reset --yes`,
# `kensaku remove-data --yes`, `kensaku config path`.

SHELL := /bin/bash
.DEFAULT_GOAL := help

BIN := target/release/kensaku
BENCH_DIR ?= $(HOME)/Projects

.PHONY: help build test lint fmt smoke bench clean

help: ## Show this help
	@printf '\nKensaku tasks:\n\n'
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-10s\033[0m %s\n", $$1, $$2}'
	@printf '\n'

build: ## Build the kensaku binary (release)
	cargo build --release -p kensaku
	@echo "built: $(BIN)"

test: ## Run the Rust test suite
	cargo test --workspace --all-targets

lint: ## Clippy with warnings as errors
	cargo clippy --workspace --all-targets -- -D warnings

fmt: ## Format Rust code
	cargo fmt --all

smoke: ## Headless end-to-end smoke test (throwaway data dir)
	cargo build -p kensaku
	tools/headless-smoke.sh target/debug/kensaku

bench: ## Run the filename-search benchmark (BENCH_DIR=...)
	cargo run --release -p kensaku-core --example bench -- "$(BENCH_DIR)"

clean: ## Remove build outputs
	cargo clean
