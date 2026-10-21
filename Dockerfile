# syntax=docker/dockerfile:1

FROM rust:1-slim AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked

FROM debian:stable-slim AS runtime
RUN useradd --system --uid 10001 --home-dir /var/lib/rustshot rustshot \
    && mkdir -p /var/lib/rustshot/data && chown -R rustshot /var/lib/rustshot
USER rustshot
WORKDIR /var/lib/rustshot
ENV RUSTSHOT_BIND=0.0.0.0:8080 \
    RUSTSHOT_STORAGE_DIR=/var/lib/rustshot/data \
    RUSTSHOT_DATABASE_URL=sqlite:///var/lib/rustshot/data/rustshot.db?mode=rwc
VOLUME /var/lib/rustshot/data
EXPOSE 8080
ENTRYPOINT ["rustshot"]
CMD ["serve"]

# Used by the release workflow: static binaries built by CI in dist/<arch>/rustshot
FROM runtime AS prebuilt
ARG TARGETARCH
COPY --chmod=755 dist/${TARGETARCH}/rustshot /usr/local/bin/rustshot

# Default target: build from source (`docker build .`, docker-compose.yml)
FROM runtime
COPY --from=build /src/target/release/rustshot /usr/local/bin/rustshot
