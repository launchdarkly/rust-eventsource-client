#!/usr/bin/env bash

set -o errexit
set -o pipefail

# We are using an older version of the toolchain for now as there is a bug with
# coverage generation. See https://github.com/rust-lang/rust/issues/79645
export RUSTUP_TOOLCHAIN=nightly-2022-01-09

# cargo-llvm-cov requires the nightly compiler to be installed
rustup toolchain install $RUSTUP_TOOLCHAIN
rustup component add llvm-tools-preview --toolchain $RUSTUP_TOOLCHAIN
cargo install cargo-llvm-cov

# generate coverage report to command line by default; otherwise allow
# CI to pass in '--html' (or other formats).

if [ -n "$1" ]; then
  cargo llvm-cov --all-features --workspace "$1"
else
  cargo llvm-cov --all-features --workspace
fi
