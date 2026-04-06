FROM rust:1.85-bookworm AS builder
RUN cargo install iroh-relay@0.97.0 --features server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/cargo/bin/iroh-relay /usr/local/bin/iroh-relay
ENTRYPOINT ["iroh-relay"]
CMD ["--dev"]
