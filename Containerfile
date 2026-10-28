# SPDX-FileCopyrightText: 2026 Sascha Brawer <sascha@brawer.ch>
# SPDX-License-Identifier: MIT
#
# This file is used in continuous integration to automatically
# build containers, invoked from .github/workflows/release.yml.
# As a developer, you do not need to build production containers.
# However, here’s how to test a change to this file:
#
#     mkdir artifacts
#     podman build -t test-container                                    \
#         --build-arg BUILD_TIMESTAMP=$(date -u +"%Y-%m-%dT%H:%M:%SZ")  \
#         --volume $(pwd)/artifacts:/artifacts                          \
#         -f Containerfile .
#     podman run -t test-container --help


# ----------------------------------------------------------------------------
#  Stage 1.1: Setup
# ----------------------------------------------------------------------------

FROM rust:1.99.1-alpine3.23 AS builder

ARG BUILD_TIMESTAMP
ARG IMAGE_NAME=brawer/osmdiffs
ARG TIPPECANOE_VERSION=2.79.0
# Commit that the "2.79.0" tag pointed to as of 2026-08-08. Pinned by
# commit, not by tag/branch name: git tags are mutable references that
# can be force-moved (accidentally or maliciously) after the fact, while
# fetching by commit SHA is content-addressed and can't silently drift.
# Update alongside TIPPECANOE_VERSION when bumping to a newer release.
ARG TIPPECANOE_COMMIT=68ab8dcc229f95b8b25877697d5e8d66783af503

# TODO(#556): Take cargo-cyclonedx from stable Alpine Linux (not edge)
# once Alpine 3.24 has been released.
RUN echo "@edge https://dl-cdn.alpinelinux.org/alpine/edge/community" >> /etc/apk/repositories && \
    apk update && \
    apk add \
      bash \
      build-base \
      cargo-cyclonedx@edge \
      cmake \
      git \
      jq \
      protoc \
      sqlite-static \
      sqlite-dev \
      zlib-static \
      zlib-dev


# ----------------------------------------------------------------------------
#  Stage 1.2: Build statically linked tippecanoe binary
# ----------------------------------------------------------------------------
#
# `make install` also installs tile-join -- a sibling binary from this
# same source tree/commit, used by pipeline::tiles::join_tiles to merge
# conflated.pmtiles' overview and detail passes (see
# pipeline::conflated_tiles' module doc comment). Not a second
# dependency to track: same repo, same commit, same static-link
# treatment, same license.

WORKDIR /build/tippecanoe

RUN git init -q . && \
    git remote add origin https://github.com/felt/tippecanoe.git && \
    git fetch --depth 1 origin "${TIPPECANOE_COMMIT}" && \
    git checkout -q FETCH_HEAD

RUN make -j"$(nproc)" \
        PREFIX=/usr/local \
        LDFLAGS="-static -static-libgcc -static-libstdc++" && \
    make install PREFIX=/usr/local && \
    strip --strip-all /usr/local/bin/tippecanoe /usr/local/bin/tile-join

# Sanity-check: confirm both binaries are truly statically linked
RUN for bin in tippecanoe tile-join; do \
        readelf -d "/usr/local/bin/$bin" 2>&1 | grep -q NEEDED \
        && (echo "✗ dynamic deps detected in $bin" && exit 1) \
        || echo "✓ no dynamic library dependencies in $bin"; \
    done


# ----------------------------------------------------------------------------
#  Stage 1.3: Build and test osmdiffs binary
# ----------------------------------------------------------------------------

WORKDIR /usr/osmdiffs

COPY Cargo.toml Cargo.lock build.rs rust-toolchain.toml .

# cargo-auditable embeds the binary's exact Rust dependency list into
# the binary itself, in a `.dep-v0` section. That lets vulnerability
# scanners (Trivy, Grype, `cargo audit bin`) find the crates inside our
# scratch image without needing our SBOM. Installed after copying
# rust-toolchain.toml, so it gets built by the same pinned toolchain.
RUN cargo install --locked cargo-auditable@0.7.7

COPY scripts/sbom scripts/sbom
COPY src src
COPY tests tests

# Tests also run through cargo-auditable: a plain `cargo test` would
# rebuild target/release/osmdiffs without the embedded dependency list
# and overwrite the binary we ship. This way, the integration tests
# exercise exactly the binary that goes into the image.
RUN cargo auditable build --release --locked
RUN cargo auditable test --release --locked

# Fail the build if the dependency list did not get embedded.
RUN readelf --section-headers target/release/osmdiffs | grep -q '\.dep-v0' \
    && echo "✓ cargo-auditable dependency data embedded in osmdiffs" \
    || (echo "✗ no .dep-v0 section in osmdiffs" && exit 1)


# ----------------------------------------------------------------------------
#  Stage 1.4: Generate the SBOM for the container
# ----------------------------------------------------------------------------
#
# Runs after both binaries (tippecanoe and osmdiffs) have been built, so a
# single script invocation can see the facts about both of them.

RUN sh scripts/sbom/generate-sbom.sh /artifacts/sbom.cdx.json


# ----------------------------------------------------------------------------
#  Stage 2: Package binaries into a scratch container
# ----------------------------------------------------------------------------

FROM scratch

ARG BUILD_TIMESTAMP
ARG VCS_REF
ARG VCS_URL

COPY --from=builder /usr/local/bin/tippecanoe /usr/local/bin/tippecanoe
COPY --from=builder /usr/local/bin/tile-join /usr/local/bin/tile-join
    
COPY --from=builder --chown=1000:1000  \
    /usr/osmdiffs/target/release/osmdiffs  \
    /app/osmdiffs

USER 1000

ENTRYPOINT ["/app/osmdiffs"]

LABEL  \
    org.opencontainers.image.authors="Sascha Brawer <sascha@brawer.ch>"  \
    org.opencontainers.image.created=$BUILD_TIMESTAMP  \
    org.opencontainers.image.description="Data pipeline for brawer/osmdiffs"  \
    org.opencontainers.image.licenses="MIT"  \
    org.opencontainers.image.revision=$VCS_REF  \
    org.opencontainers.image.source=$VCS_URL  \
    org.opencontainers.image.vendor="alltheplaces.xyz"
