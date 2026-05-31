.PHONY: build run clean

CARGO ?= cargo
TIMENAV_LIB := ../../target/debug/libtimenav.so
CFLAGS := -Wall -Wextra -O2 -g -I../../include

build:
	cd ../.. && $(CARGO) build --lib
	$(CC) $(CFLAGS) -o demo demo.c -L../../target/debug -ltimenav -Wl,-rpath,../../target/debug

run: build
	./demo

clean:
	rm -f demo
