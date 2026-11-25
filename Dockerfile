# syntax=docker/dockerfile:1

# --- Build -------------------------------------------------------------------
FROM rust:1.95-bookworm AS build
WORKDIR /app

# Build dependencies first, so code changes don't rebuild them.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src \
 && echo "fn main() {}" > src/main.rs \
 && touch src/lib.rs \
 && cargo build --release --locked --no-default-features --features server,avif \
 && rm -rf src

COPY src ./src
RUN touch src/main.rs src/lib.rs \
 && cargo build --release --locked --no-default-features --features server,avif

# --- Run ---------------------------------------------------------------------
# Distroless: no shell or package manager, CA certificates included, and a
# non-root user.
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /app/target/release/ansel /usr/local/bin/ansel
USER nonroot
ENV PORT=8080
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/ansel"]
