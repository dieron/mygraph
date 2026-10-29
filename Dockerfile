# MyGraph server image: build with Rust, run on a small non-root base with no shell.
#
#   docker build -t mygraph .
#   docker run -d -p 7420:7420 -v mygraph-data:/data -e MYGRAPH_ADMIN_KEY=<secret> mygraph
#
# Settings come from MYGRAPH_* environment variables (crates/mygraph-server/src/config.rs);
# the container listens on all interfaces, which the server only allows with an API key.

# ---- build ----------------------------------------------------------------------------------
FROM rust:1.99-bookworm AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p mygraph-server \
 && install -D target/release/mygraph /out/mygraph \
 && install -d -o 65532 -g 65532 /out/data

# ---- run ------------------------------------------------------------------------------------
# Debian 12 with glibc and the C runtime, user "nonroot" (65532), no shell or package manager.
FROM gcr.io/distroless/cc-debian12:nonroot
LABEL org.opencontainers.image.title="MyGraph" \
      org.opencontainers.image.description="Graph database with evidence, for AI agents (MCP and REST)" \
      org.opencontainers.image.licenses="MIT OR Apache-2.0"
COPY --from=build /out/mygraph /usr/local/bin/mygraph
COPY --from=build --chown=65532:65532 /out/data /data
ENV MYGRAPH_LISTEN=0.0.0.0:7420 \
    MYGRAPH_DATA_DIR=/data
VOLUME ["/data"]
EXPOSE 7420
USER 65532:65532
HEALTHCHECK --interval=30s --timeout=6s --start-period=60s --retries=3 \
  CMD ["/usr/local/bin/mygraph", "health"]
ENTRYPOINT ["/usr/local/bin/mygraph"]
CMD ["serve"]
