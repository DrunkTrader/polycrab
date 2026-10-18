.PHONY: test build fmt-check lint docker-build

test:
	cargo test --locked --offline

build:
	cargo build --release --locked

fmt-check:
	cargo fmt --all -- --check

lint:
	cargo clippy --all-targets --locked -- -D warnings

docker-build:
	docker build -t polycrab .
