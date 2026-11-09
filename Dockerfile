# CPU-only ZeroFlux node + CPU miners. No GPU runtime is installed: blocks
# must verify without CUDA/ROCm (spec section 8).
FROM rust:1.90-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p zfx-node -p zfx-wallet -p zpu-cli \
 && make -C miner/cuda cpu

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/zfx-node /src/target/release/zfx-wallet /src/target/release/zpu-cli /usr/local/bin/
COPY --from=build /src/miner/cuda/build/zpu-miner /usr/local/bin/zpu-miner
COPY --from=build /src/explorer/dist /opt/zfx/explorer
COPY --from=build /src/tests/vectors /opt/zfx/vectors
EXPOSE 9440 9441
ENTRYPOINT ["zfx-node"]
CMD ["--data-dir", "/data", "--explorer-dir", "/opt/zfx/explorer"]
