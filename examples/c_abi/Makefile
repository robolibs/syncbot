.PHONY: build run clean

CARGO ?= cargo
TIMENAV_LIB := ../../target/debug/libtimenav.so
INCLUDE := ../..
CFLAGS := -Wall -Wextra -O2 -g -I.

build: $(TIMENAV_LIB) demo

$(TIMENAV_LIB):
	cd ../.. && $(CARGO) build --lib

demo: demo.c $(TIMENAV_LIB)
	$(CC) $(CFLAGS) -o demo demo.c -L../../target/debug -ltimenav -Wl,-rpath,../../target/debug

run: build
	./demo

clean:
	rm -f demo
