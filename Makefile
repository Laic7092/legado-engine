.PHONY: build test test-c lint fmt clean

build:
	cargo build --release --lib

test:
	cargo test

lint:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings

test-c: build
	$(CC) -O2 -I. tests/c_abi.c target/release/liblegado_engine.a -lpthread -ldl -lm -o target/c_abi
	./target/c_abi

fmt:
	cargo fmt

clean:
	cargo clean
