# Docker and production AOT

ZZ publishes three related images from every release:

| Tag | Contents | Use |
|---|---|---|
| `zz:<version>` | `zz` + `zz-lsp` only | CLI, CI checks, tools |
| `zz:<version>-builder` | toolchain + Clang + dev headers | compile AOT apps |
| `zz:<version>-runtime` | glibc, libgcc, curl, SQLite, OpenSSL, CA roots | run dynamic AOT apps |

Pin the builder and runtime tags to the same released version. `latest`,
`latest-builder`, and `latest-runtime` are moving aliases: convenient for a
manual look, never acceptable in a production Dockerfile.

## Recommended: builder + runtime

Application Dockerfiles do not install Clang or C headers:

```dockerfile
FROM ghcr.io/zaidejjo/zz:<version>-builder AS build
WORKDIR /src
COPY . .
RUN zz check src/ && zz build -p src/main.zz -o /out/app

FROM ghcr.io/zaidejjo/zz:<version>-runtime
COPY --from=build /out/app /app
USER 10001:10001
ENV PORT=8080
EXPOSE 8080
STOPSIGNAL SIGTERM
ENTRYPOINT ["/app"]
```

`zz build -p` produces a dynamically linked native binary. The runtime
image contains the shared libraries that binary needs, so the final stage
stays free of compilers, linkers, and development headers.

The builder and runtime images share one Ubuntu base, pinned by digest, so a
binary compiled by a released builder always finds the library versions it
was tested against. The runtime image already defaults to a non-root numeric
user; override `USER` only when the application has a specific account
requirement.

## Pinning and verification

Both tags are multi-arch (`linux/amd64`, `linux/arm64`). For a truly
immutable deploy, pin the index digest that the tag resolved to:

```dockerfile
FROM ghcr.io/zaidejjo/zz:<version>-builder@sha256:<builder-digest> AS build
FROM ghcr.io/zaidejjo/zz:<version>-runtime@sha256:<runtime-digest>
```

Resolve digests without pulling the image:

```bash
docker buildx imagetools inspect ghcr.io/zaidejjo/zz:<version>-runtime
```

Every published digest is signed keylessly with Sigstore (GitHub OIDC, no
long-lived key) and carries SLSA provenance plus an SBOM attestation:

```bash
cosign verify ghcr.io/zaidejjo/zz:<version>-runtime \
  --certificate-identity-regexp \
    'https://github.com/zaidejjo/zz/.github/workflows/docker-publish.yml@.*' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
docker buildx imagetools inspect ghcr.io/zaidejjo/zz:<version>-runtime \
  --format '{{json .Provenance}}'
```

Verification binds the image to this repository's publish workflow; an image
pushed from a fork or a local build will not verify.

## Why not compile in the runtime stage?

A single-stage image must keep Clang, the linker, headers, and package
manager metadata in production. That increases image size and attack
surface for no runtime benefit. The builder/runtime split is the same
pattern used by Go, Rust, and JVM images: heavy build environment, minimal
runtime environment.

## Static binaries

A fully static binary would allow a distroless or `scratch` runtime, but
`zz build --static` is not the recommended production path yet: AOT output
may still need static curl, SQLite, and OpenSSL, and static glibc has
DNS/NSS caveats. Use the builder/runtime pair until the static path is
covered by CI for TLS, DNS, SQLite, and Postgres.

## Image versions

The publish workflow produces matching tags for both architectures:

```text
ghcr.io/<owner>/zz:<version>-builder
ghcr.io/<owner>/zz:<version>-runtime
ghcr.io/<owner>/zz:sha-<commit>-builder
ghcr.io/<owner>/zz:sha-<commit>-runtime
```

The legacy `zz:<version>` tag still carries the released CLI. The builder and
runtime tags are only published together for the same version, so mixing a
`-builder` from one release with a `-runtime` from another is not a supported
combination.

The builder is based on the same pinned Ubuntu 24.04 digest as the release
toolchain and the runtime image. Mixing a newer builder with an older runtime
base is the library-drift failure this split avoids. After publishing, the
workflow compiles `docker/smoke/hello.zz` in the new builder and runs the
binary on the new runtime; a failed smoke test marks the publish failed.

## Related

- [CLI reference](../tooling/cli-reference.md)
- [Install](../getting-started/install.md)
- [VM vs native parity](parity.md)
