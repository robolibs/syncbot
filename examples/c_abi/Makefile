.PHONY: build run clean

CARGO ?= cargo
SYNCBOT_LIB := ../../target/debug/libsyncbot.so
CFLAGS := -Wall -Wextra -O2 -g -I../../include

build:
	cd ../.. && $(CARGO) build --lib
	$(CC) $(CFLAGS) -o demo demo.c -L../../target/debug -lsyncbot -Wl,-rpath,../../target/debug

run: build
	./demo

clean:
	rm -f demo
