.PHONY: build test test-c fmt clean

build:
	cargo build --release --lib

test:
	cargo test

test-c: build
	$(CC) -O2 -I. tests/c_abi.c target/release/liblegado_engine.a -lpthread -ldl -lm -o target/c_abi
	./target/c_abi

fmt:
	rustfmt --edition 2021 src/abi.rs src/pipeline.rs src/service/*.rs tests/*.rs examples/*.rs

clean:
	cargo clean
