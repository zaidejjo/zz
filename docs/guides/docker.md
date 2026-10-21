# Docker and production AOT

ZZ publishes three related images from every release:

| Tag | Contents | Use |
|---|---|---|
| `zz:<version>` | `zz` + `zz-lsp` only | CLI, CI checks, tools |
| `zz:<version>-builder` | toolchain + Clang + dev headers | compile AOT apps |
| `zz:<version>-runtime` | glibc, libgcc, curl, SQLite, OpenSSL, CA roots | run dynamic AOT apps |

Pin the builder and runtime tags to the same version. `latest-builder` and
`latest-runtime` are moving preview aliases; do not use them in production.

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

The runtime image already defaults to a non-root numeric user. Override
`USER` only when the application has a specific account requirement.

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

The release workflow publishes matching tags for both architectures:

```text
ghcr.io/<owner>/zz:<version>-builder
ghcr.io/<owner>/zz:<version>-runtime
ghcr.io/<owner>/zz:sha-<commit>-builder
ghcr.io/<owner>/zz:sha-<commit>-runtime
```

The builder is based on Ubuntu 24.04, matching the release toolchain glibc
and the runtime image. Mixing a newer builder with an older runtime base is
the library-drift failure this split avoids.

## Related

- [CLI reference](../tooling/cli-reference.md)
- [Install](../getting-started/install.md)
- [VM vs native parity](parity.md)
