.PHONY: check run

# Requires the peerbus and datapod Python bindings from the sibling projects.
check:
	python3 -c 'import ast, pathlib; ast.parse(pathlib.Path("adapter.py").read_text())'

run:
	python3 adapter.py
