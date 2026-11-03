# MyGraph server image: build with Rust, run on a small non-root base with no shell.
#
#   docker build -t mygraph .
#   docker run -d -p 7420:7420 -v mygraph-data:/data -e MYGRAPH_ADMIN_KEY=<secret> mygraph
#
# Settings come from MYGRAPH_* environment variables (crates/mygraph-server/src/config.rs);
# the container listens on all interfaces, which the server only allows with an API key.
# The embedding model for semantic search (135 MB) is part of the image, so the server never
# downloads anything; build with --build-arg SEMANTIC_MODEL=none for an image without it (search
# then matches words only).

ARG SEMANTIC_MODEL=multilingual-e5-small

# ---- embedding model ------------------------------------------------------------------------
# Its own stage, so source changes do not download it again.
FROM rust:1.99-bookworm AS model
ARG SEMANTIC_MODEL
COPY deploy/fetch-model.sh /fetch-model.sh
RUN mkdir -p /models \
 && if [ "$SEMANTIC_MODEL" != none ]; then sh /fetch-model.sh "/models/$SEMANTIC_MODEL"; fi

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
ARG SEMANTIC_MODEL
COPY --from=build /out/mygraph /usr/local/bin/mygraph
COPY --from=build --chown=65532:65532 /out/data /data
COPY --from=model /models /usr/local/share/mygraph/models
ENV MYGRAPH_LISTEN=0.0.0.0:7420 \
    MYGRAPH_DATA_DIR=/data \
    MYGRAPH_MODEL_DIR=/usr/local/share/mygraph/models/$SEMANTIC_MODEL
VOLUME ["/data"]
EXPOSE 7420
USER 65532:65532
HEALTHCHECK --interval=30s --timeout=6s --start-period=60s --retries=3 \
  CMD ["/usr/local/bin/mygraph", "health"]
ENTRYPOINT ["/usr/local/bin/mygraph"]
CMD ["serve"]
