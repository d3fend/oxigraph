FROM --platform=$BUILDPLATFORM rust:1-bookworm AS builder
ARG BUILDARCH
ARG TARGETARCH

COPY . /oxigraph
WORKDIR /oxigraph

# Install optional enterprise CA certs from .local as early as possible.
RUN set -eux; \
    cert_count="$(find /oxigraph/.local -type f \( -name '*.crt' -o -name '*.pem' \) 2>/dev/null | wc -l)"; \
    if [ "$cert_count" -gt 0 ]; then \
        echo "Installing ${cert_count} enterprise CA certificate(s) from /oxigraph/.local"; \
        find /oxigraph/.local -type f \( -name '*.crt' -o -name '*.pem' \) \
            -exec sh -c 'for cert_path; do cert_name="$(basename "${cert_path%.*}")"; install -m 0644 "$cert_path" "/usr/local/share/ca-certificates/${cert_name}.crt"; done' sh {} +; \
        if command -v update-ca-certificates >/dev/null 2>&1; then \
            update-ca-certificates; \
        fi; \
    fi

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates libclang-dev clang && \
    if [ "$BUILDARCH" != "$TARGETARCH" ] && [ "$TARGETARCH" = "arm64" ] ; then \
        apt-get install -y --no-install-recommends g++-aarch64-linux-gnu && \
        rustup target add aarch64-unknown-linux-gnu ; \
    fi && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /oxigraph/cli
RUN if [ "$BUILDARCH" != "$TARGETARCH" ] && [ "$TARGETARCH" = "arm64" ] ; then \
        export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc && \
        export BINDGEN_EXTRA_CLANG_ARGS="--sysroot /usr/aarch64-linux-gnu" && \
        cargo build --release --target aarch64-unknown-linux-gnu --no-default-features --features rustls-native,geosparql,rdf-12 && \
        mv /oxigraph/target/aarch64-unknown-linux-gnu/release/oxigraph /oxigraph/target/release/oxigraph ; \
    else \
        cargo build --release --no-default-features --features rustls-native,geosparql,rdf-12 ; \
    fi

FROM gcr.io/distroless/cc-debian12
COPY --from=builder /etc/ssl/certs /etc/ssl/certs
COPY --from=builder /usr/local/share/ca-certificates /usr/local/share/ca-certificates
COPY --from=builder /oxigraph/target/release/oxigraph /usr/local/bin/oxigraph
ENV SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
VOLUME ["/data"]
EXPOSE 7878
ENTRYPOINT ["/usr/local/bin/oxigraph"]
CMD ["serve", "--location", "/data", "--bind", "0.0.0.0:7878"]
