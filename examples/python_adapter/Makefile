.PHONY: check run

# Parses, then verifies the packed header layouts still match the sizes the
# core pins in canonical_header_sizes_are_frozen. Needs no peerbus bindings.
check:
	python3 -c 'import ast, pathlib; ast.parse(pathlib.Path("adapter.py").read_text())'
	python3 adapter.py --self-check

# Requires the peerbus and datapod Python bindings from the sibling projects.
run:
	python3 adapter.py
