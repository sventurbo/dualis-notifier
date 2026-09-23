FROM rust:1-bookworm AS build
WORKDIR /src

COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked \
    && mkdir /data

# Distroless has no shell, so the binary checks on its own schedule
# (CHECK_INTERVAL_MINUTES) instead of relying on cron.
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /src/target/release/dualis-notifier /usr/local/bin/dualis-notifier
COPY --from=build --chown=65532:65532 /data /data

ENV CHECK_INTERVAL_MINUTES=15 \
    DATA_DIR=/data
VOLUME /data
WORKDIR /data
ENTRYPOINT ["/usr/local/bin/dualis-notifier"]
