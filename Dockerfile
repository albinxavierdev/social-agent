# syntax=docker/dockerfile:1
# One image, two roles: `content-agent-api` and `content-agent-worker` (choose with the container command).
FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY migrations ./migrations
COPY src ./src
RUN cargo build --release --locked --bins

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates tini && rm -rf /var/lib/apt/lists/* \
 && useradd --uid 10001 --no-create-home --shell /usr/sbin/nologin app
COPY --from=build /src/target/release/content-agent-api /usr/local/bin/
COPY --from=build /src/target/release/content-agent-worker /usr/local/bin/
USER 10001
EXPOSE 8080
ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["content-agent-api"]
# No secrets are baked into the image: everything comes from the environment / secret manager at runtime.

# ---------------------------------------------------------------------------------------------
# Separate Obscura browser-worker pool (spec §17): the worker image plus the Obscura binary.
#   docker build --target obscura-worker --build-arg OBSCURA_URL=<release tarball URL> -t content-agent-obscura-worker .
# Take the URL from https://github.com/h4ckf0r0day/obscura/releases (linux x86_64, "render" variant). The archive
# must contain a single `obscura` executable at its root.
FROM runtime AS obscura-worker
ARG OBSCURA_URL
USER root
RUN test -n "$OBSCURA_URL" || (echo "set --build-arg OBSCURA_URL=<obscura release tarball>" && exit 1)
ADD ${OBSCURA_URL} /tmp/obscura.tar.gz
RUN tar -xzf /tmp/obscura.tar.gz -C /usr/local/bin obscura && chmod 0755 /usr/local/bin/obscura && rm /tmp/obscura.tar.gz
USER 10001
ENV OBSCURA_BIN=/usr/local/bin/obscura
CMD ["content-agent-worker"]
