# Linux-хост сборки для частей Servo, которые на Windows не собираются
# (SpiderMonkey/mozjs: сборочная система Mozilla — python + make + sh).
# Собирает под цели `<арх>-unknown-freeos` тем же набором, что и xtask:
# sysroot из build/toolchain/sysroot (монтируется), обёртка freeos-cc —
# собирается здесь под Linux из crates/freeos-cc.
#
#   docker build -t fossai-builder ports/servo/builder
#   docker run --rm -v <проект>:/work fossai-builder <команда>
FROM debian:trixie

ARG LLVM=22
ARG RUST_NIGHTLY=nightly-2026-08-11

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl gnupg git make m4 perl python3 python3-venv \
        pkg-config file xz-utils unzip wget \
    && rm -rf /var/lib/apt/lists/*

# LLVM той же ветки, что набор FreeOS на Windows (libc++ 22): clang, lld,
# llvm-ar, libclang для bindgen.
RUN wget -qO /etc/apt/trusted.gpg.d/apt.llvm.org.asc https://apt.llvm.org/llvm-snapshot.gpg.key \
    && echo "deb http://apt.llvm.org/trixie/ llvm-toolchain-trixie-${LLVM} main" > /etc/apt/sources.list.d/llvm.list \
    && apt-get update && apt-get install -y --no-install-recommends \
        clang-${LLVM} lld-${LLVM} llvm-${LLVM} llvm-${LLVM}-dev libclang-${LLVM}-dev \
    && rm -rf /var/lib/apt/lists/*
ENV PATH=/usr/lib/llvm-${LLVM}/bin:$PATH \
    LIBCLANG_PATH=/usr/lib/llvm-${LLVM}/lib

# Rust той же ночной сборки, что на ПК (правки std привязаны к её rust-src).
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain ${RUST_NIGHTLY} \
    && rustup component add rust-src --toolchain ${RUST_NIGHTLY} \
    && rustc -vV

WORKDIR /work
