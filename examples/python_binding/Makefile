.PHONY: build check run yard clean

TOP := ../..
# maturin needs a venv to install into, and `maturin develop` insists on one.
# Building a wheel and unpacking it instead keeps this hermetic and leaves the
# caller's environment alone — which is what `make ci` at the root wants.
STAGE := $(CURDIR)/.stage
WHEEL_DIR := $(STAGE)/wheel
PYTHON ?= python3

# Build the extension into a throwaway directory and put it on PYTHONPATH.
$(STAGE)/.stamp:
	@rm -rf "$(STAGE)"
	@mkdir -p "$(WHEEL_DIR)"
	@cd $(TOP) && maturin build --quiet --features python --out "$(WHEEL_DIR)"
	@$(PYTHON) -m zipfile -e "$$(ls "$(WHEEL_DIR)"/*.whl | head -1)" "$(STAGE)/lib"
	@touch "$@"

build: $(STAGE)/.stamp

# The binding test: builds a workspace, plans over it, runs the claim loop.
# Everything here goes through the bindings, nothing through Rust.
check: build
	@PYTHONPATH="$(STAGE)/lib" $(PYTHON) test_bindings.py

# The original smoke script, kept because it covers the parser surface.
run: build
	@PYTHONPATH="$(STAGE)/lib" $(PYTHON) example.py

# The packhouse-yard scenario in rerun. Needs `rerun-sdk`; the repo's
# `nix develop` shell provides a version matching the Rust crate.
yard: build
	@PYTHONPATH="$(STAGE)/lib" $(PYTHON) packhouse_yard.py

clean:
	@rm -rf "$(STAGE)"
