.PHONY: build run clean

# Build the timenav Python extension with maturin and run the smoke script.
# Requires `maturin` (pip install maturin) and `python3.9+`.

build:
	cd ../.. && maturin develop --features python-extension

run: build
	python3 example.py

clean:
	cd ../.. && cargo clean
