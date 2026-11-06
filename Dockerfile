FROM rust:1.99.0-bookworm AS builder

WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install --no-install-recommends -y ca-certificates libgcc-s1 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --uid 10001 polycrab

WORKDIR /app
COPY --from=builder /src/target/release/polycrab /usr/local/bin/polycrab
COPY config.json config.yaml.example ./

USER polycrab
ENTRYPOINT ["polycrab"]
