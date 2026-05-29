SHELL := /bin/bash

PROJECT_NAME := $(shell sed -n '/^[[:space:]]*[^#\[[:space:]]/p' PROJECT | head -1 | tr -d '[:space:]')
PROJECT_VERSION := $(shell sed -n '/^[[:space:]]*[^#\[[:space:]]/p' PROJECT | sed -n '2p' | tr -d '[:space:]')
ifeq ($(PROJECT_NAME),)
    $(error Error: PROJECT file not found or invalid)
endif

TOP_DIR := $(CURDIR)
CARGO := cargo
EXAMPLE ?= serve_workspace
RUN_FEATURES ?= rest
RUN_ARGS ?= examples/fixed
RUN_FEATURE_ARGS := $(if $(strip $(RUN_FEATURES)),--features "$(RUN_FEATURES)",)

$(info ------------------------------------------)
$(info Project: $(PROJECT_NAME) v$(PROJECT_VERSION))
$(info ------------------------------------------)

.PHONY: build b compile c run r fixed-map test t check check-rest check-robo check-transports fmt bench clean help h test-python c-demo

build:
	@$(CARGO) build --lib --examples

b: build

compile:
	@$(CARGO) clean
	@$(MAKE) build

c: compile

run:
	@$(CARGO) run $(RUN_FEATURE_ARGS) --example $(EXAMPLE) -- $(RUN_ARGS)

r: run

fixed-map:
	@$(CARGO) run --example generate_fixed -- examples/fixed

test:
	@$(CARGO) test --all-targets

t: test

test-python:
	@$(CARGO) check --features python

c-demo:
	@$(MAKE) -C examples/c_abi run

check:
	@$(CARGO) check --all-targets

check-rest:
	@$(CARGO) check --features rest

check-robo:
	@$(CARGO) check --features robo

check-transports:
	@$(CARGO) check --features "rest robo"

fmt:
	@$(CARGO) fmt --all

bench:
	@$(CARGO) bench

clean:
	@$(CARGO) clean

help:
	@echo
	@echo "Usage: make [target]"
	@echo
	@echo "Available targets:"
	@echo "  build        Build the library and examples"
	@echo "  compile      Clean and rebuild"
	@echo "  run          Run the workspace REST server (loads RUN_ARGS=examples/fixed by default)"
	@echo "  fixed-map    Generate the fixed example zone map in examples/fixed"
	@echo "  test         Run all tests"
	@echo "  test-python  Run tests with Python bindings enabled"
	@echo "  check        Run cargo check on all targets"
	@echo "  check-rest   Check the optional Axum REST adapter"
	@echo "  check-robo   Check the optional Zenoh robotics adapter"
	@echo "  check-transports Check REST and Zenoh adapters together"
	@echo "  fmt          Format the workspace"
	@echo "  bench        Run benchmarks"
	@echo "  c-demo       Build and run the C ABI example"
	@echo "  clean        Remove Cargo build artifacts"
	@echo
	@echo "Examples:"
	@echo "  make run"
	@echo "  make run RUN_ARGS=\"examples/fixed 127.0.0.1:8081\""
	@echo "  make run EXAMPLE=route_planning RUN_FEATURES= RUN_ARGS="
	@echo

h: help
