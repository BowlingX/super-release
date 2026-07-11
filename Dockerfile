FROM alpine:3 AS pick
ARG TARGETARCH
WORKDIR /pick
COPY artifacts/ ./artifacts/
RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) arch=x86_64 ;; \
      arm64) arch=aarch64 ;; \
      *) echo "unsupported TARGETARCH: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    tar xzf "artifacts/super-release-linux-${arch}-musl/super-release-linux-${arch}-musl.tar.gz" -C /; \
    test -x /super-release

FROM node:24-alpine@sha256:a0b9bf06e4e6193cf7a0f58816cc935ff8c2a908f81e6f1a95432d679c54fbfd

LABEL org.opencontainers.image.source="https://github.com/BowlingX/super-release" \
      org.opencontainers.image.description="A fast semantic-release alternative for monorepos" \
      org.opencontainers.image.licenses="MIT"

RUN apk add --no-cache git ca-certificates && corepack enable
ENV COREPACK_ENABLE_DOWNLOAD_PROMPT=0

ARG SR_VERSION
ENV SUPER_RELEASE_VERSION=${SR_VERSION}

COPY --from=pick /super-release /usr/local/bin/super-release
COPY entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/super-release /usr/local/bin/entrypoint.sh

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
