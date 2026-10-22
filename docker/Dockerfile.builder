# Official ZZ AOT builder image.
#
# Published by .github/workflows/docker-publish.yml as
#   ghcr.io/<owner>/zz:<version>-builder
#   ghcr.io/<owner>/zz:latest-builder   (moving preview alias)
# and mirrored to Docker Hub when configured.
#
# This image owns the build-time dependencies every ZZ app would otherwise
# repeat: the released `zz`/`zz-lsp` binaries, Clang, the linker, libc
# headers, and the dev headers for the libraries AOT output links (curl,
# SQLite, OpenSSL). Application Dockerfiles only need:
#
#   FROM ghcr.io/<owner>/zz:<version>-builder AS build
#   WORKDIR /src
#   COPY . .
#   RUN zz build -p src/main.zz -o /out/app
#
# Keep the builder and runtime tags pinned to the same release. The runtime
# image is built from the same Ubuntu base so dynamic library versions match.
#
# Local build (requires a published toolchain image for this arch):
#   docker build -f docker/Dockerfile.builder \
#     --build-arg TOOLCHAIN_IMAGE=ghcr.io/zaidejjo/zz:0.4.0 -t zz:builder .

ARG TOOLCHAIN_IMAGE

# The toolchain image only carries the release binaries. Copy them out so
# the builder can own its entrypoint and package set.
FROM ${TOOLCHAIN_IMAGE} AS tool

# Same pinned base as Dockerfile.runtime and Dockerfile.toolchain (`ubuntu:24.04`
# at 2026-10-21). A digest keeps a moving distro tag from silently changing the
# linker or glibc under an already-released builder. Refresh deliberately with
#   docker buildx imagetools inspect ubuntu:24.04
# and update all three Dockerfiles together; `docker-publish` re-runs the
# builder -> runtime smoke job on every publish.
FROM ubuntu:24.04@sha256:68ee22004a9109b44973313b02ff7b1de250f1de0f5ecb628d73f579aaff33ac

# `libc6-dev` is explicit because `--no-install-recommends` may omit it
# even though Clang needs the C headers to compile generated code.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        clang \
        binutils \
        pkg-config \
        libc6-dev \
        libcurl4-openssl-dev \
        libsqlite3-dev \
        libssl-dev \
        zlib1g-dev \
    && rm -rf /var/lib/apt/lists/*

COPY --from=tool /usr/local/bin/zz /usr/local/bin/zz
COPY --from=tool /usr/local/bin/zz-lsp /usr/local/bin/zz-lsp
# `zz build` links `libzz_native_rt.a` (and the static libstd rlib) from
# <exe>/../lib/zz. Without it the builder is unusable — see Dockerfile.toolchain.
COPY --from=tool /usr/local/lib/zz /usr/local/lib/zz

# Fail the image build if the copied compiler cannot run in this base.
RUN zz --version

WORKDIR /src
ENTRYPOINT []
CMD ["zz", "--version"]
