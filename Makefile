SHELL := /bin/bash

PROJECT_NAME := $(shell if [ -f PROJECT ]; then sed -n '/^[[:space:]]*[^#\[[:space:]]/p' PROJECT | head -1 | tr -d '[:space:]'; else sed -n 's/^[[:space:]]*name[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' Cargo.toml | head -1; fi)
PROJECT_VERSION := $(shell if [ -f PROJECT ]; then sed -n '/^[[:space:]]*[^#\[[:space:]]/p' PROJECT | sed -n '2p' | tr -d '[:space:]'; else sed -n 's/^[[:space:]]*version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' Cargo.toml | head -1; fi)
ifeq ($(PROJECT_NAME),)
    $(error Error: PROJECT file not found or invalid)
endif

TOP_DIR := $(CURDIR)
CARGO := cargo
# `bash` on PATH can be a shim (a login-shell wrapper, say), which breaks the
# usecase scripts on BASH_SOURCE. Prefer a real one.
BASH := $(shell for b in /usr/bin/bash /bin/bash "$$(command -v bash)"; do \
	if [ -x "$$b" ] && "$$b" -c '[ -n "$${BASH_VERSION:-}" ]' 2>/dev/null; then echo "$$b"; break; fi; \
	done)
EXAMPLE ?= serve_workspace
RUN_FEATURES ?= rest xmlt
RUN_ARGS ?= examples/fixed
RUN_FEATURE_ARGS := $(if $(strip $(RUN_FEATURES)),--features "$(RUN_FEATURES)",)
FUZZ_TARGETS ?= workspace_push canonical_datapod
FUZZ_TARGET ?= workspace_push
FUZZ_SECONDS ?= 60

HAS_REL := $(shell command -v git-rel 2>/dev/null)

$(info ------------------------------------------)
$(info Project: $(PROJECT_NAME) v$(PROJECT_VERSION))
$(info ------------------------------------------)

.PHONY: build b compile c run r test t test-peerbus test-all test-usecase-rest test-usecase check check-peerbus check-all check-python-adapter fmt bench clean ci viz fuzz fuzz-all fixed-map bind bind-c bind-py help h

build:
	@$(CARGO) build --lib

b: build

compile:
	@$(CARGO) clean
	@$(MAKE) build

c: compile

run:
	@if [ "$(DAEMON)" = "1" ]; then \
		mkdir -p target; \
		$(CARGO) build $(RUN_FEATURE_ARGS) --example $(EXAMPLE) && \
		nohup target/debug/examples/$(EXAMPLE) $(RUN_ARGS) \
			> target/$(PROJECT_NAME)-$(EXAMPLE).log 2>&1 < /dev/null & \
		pid=$$!; \
		echo $$pid > target/$(PROJECT_NAME)-$(EXAMPLE).pid; \
		echo "daemon started: pid=$$pid"; \
		echo "log: target/$(PROJECT_NAME)-$(EXAMPLE).log"; \
		echo "stop: kill \$$(cat target/$(PROJECT_NAME)-$(EXAMPLE).pid)"; \
	else \
		$(CARGO) run $(RUN_FEATURE_ARGS) --example $(EXAMPLE) -- $(RUN_ARGS); \
	fi

r: run

test:
	@$(CARGO) test --all-targets

t: test

test-peerbus:
	@$(CARGO) test --all-targets --features peerbus

test-all:
	@$(CARGO) test --all-targets --features "peerbus rest xmlt"

test-usecase-rest:
	@$(CARGO) build --example serve_workspace --features "rest xmlt"
	@$(BASH) tests/usecase/rest.sh

test-usecase: test-usecase-rest

check:
	@$(CARGO) check --all-targets

check-peerbus:
	@$(CARGO) check --all-targets --features peerbus

check-all:
	@$(CARGO) check --all-targets --features "peerbus rest xmlt"

check-python-adapter:
	@$(MAKE) -C examples/python_adapter check

fmt:
	@$(CARGO) fmt --package $(PROJECT_NAME)

clean:
	@$(CARGO) clean

# Everything that would gate a merge, in one command. Codeberg Actions is not
# enabled for this repository, so this is the gate — run it before pushing.
ci:
	@echo "== check (all adapters)"      && $(CARGO) check --all-targets --features "peerbus rest xmlt"
	@echo "== check (python bindings)"   && $(CARGO) check --features python
	@echo "== check (rerun visualizer)"  && $(CARGO) check --example fleet_viz --features "rerun-viz peerbus"
	@echo "== clippy"                    && $(CARGO) clippy --all-targets --features "peerbus rest xmlt" -- -D warnings
	@echo "== fmt"                       && $(CARGO) fmt --package $(PROJECT_NAME) -- --check
	@echo "== tests"                     && $(CARGO) test --all-targets --features "peerbus rest xmlt"
	@echo "== python adapter"            && $(MAKE) --no-print-directory check-python-adapter
	@echo "== live REST/XML battery"     && $(MAKE) --no-print-directory test-usecase-rest
	@echo
	@echo "ci: all green"

viz:
	@$(CARGO) run --example fleet_viz --features "rerun-viz peerbus" -- $(RUN_ARGS)

# cargo-fuzz needs nightly, which the default dev shell deliberately does not
# provide; flake.nix carries a `fuzz` shell for it. FUZZ_TARGET picks one,
# FUZZ_SECONDS how long to run. Corpora persist under fuzz/corpus/.
fuzz:
	@nix develop .#fuzz --command bash -c \
		'cd fuzz && cargo fuzz run $(FUZZ_TARGET) -- \
			-max_total_time=$(FUZZ_SECONDS) -rss_limit_mb=4096'

fuzz-all:
	@for target in $(FUZZ_TARGETS); do \
		echo "=== fuzzing $$target for $(FUZZ_SECONDS)s"; \
		$(MAKE) fuzz FUZZ_TARGET=$$target FUZZ_SECONDS=$(FUZZ_SECONDS) || exit 1; \
	done

fixed-map:
	@$(CARGO) run --example generate_fixed

bind: bind-c bind-py

bind-c:
	@$(CARGO) build --lib
	@cbindgen --config cbindgen.toml --crate $(PROJECT_NAME) \
		--output include/$(PROJECT_NAME).h

bind-py:
	@maturin build --features python

release:
	@if [ -z "$(HAS_REL)" ]; then \
		echo "git-rel is not installed. Please install it first."; \
		exit 1; \
	fi
	@if [ -z "$(TYPE)" ]; then \
		echo "Release type not specified. Use 'make release TYPE=[patch|minor|major|m.m.p]'"; \
		exit 1; \
	fi
	@git rel $(TYPE)

help:
	@echo
	@echo "Usage: make [target]"
	@echo
	@echo "Available targets:"
	@echo "  build        Build the library"
	@echo "  compile      Clean and rebuild"
	@echo "  run          Run the workspace server"
	@echo "  test         Run all tests"
	@echo "  test-peerbus Test the canonical peerbus core/client"
	@echo "  test-all     Test all transport adapters"
	@echo "  test-usecase-rest Run the live curl REST/XML battery from misc/USECASE.typ"
	@echo "  test-usecase Run the live transport battery from misc/USECASE.typ"
	@echo "  ci           Everything that gates a merge (run before pushing)"
	@echo "  viz          Live rerun view of the running fleet"
	@echo "  fuzz         Fuzz one target (FUZZ_TARGET=, FUZZ_SECONDS=)"
	@echo "  fuzz-all     Fuzz every target in turn"
	@echo "  fixed-map    Regenerate the examples/fixed workspace"
	@echo "  bind         Generate both C and Python bindings"
	@echo "  check        Run cargo check on all targets"
	@echo "  check-peerbus Check the canonical peerbus core/client"
	@echo "  check-all    Check all transport adapters"
	@echo "  check-python-adapter Parse + header-layout self-check for the Python adapter"
	@echo "  fmt          Format the workspace"
	@echo "  clean        Remove Cargo build artifacts"
	@echo "  release      Release a new version"
	@echo
	@echo "Examples:"
	@echo "  make run"
	@echo "  make run DAEMON=1"
	@echo "  make run RUN_ARGS=\"examples/fixed 0.0.0.0:8081\""
	@echo

h: help
