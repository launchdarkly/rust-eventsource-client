#!/usr/bin/env bash

set -o errexit
set -o pipefail

rustup component add llvm-tools-preview
cargo install cargo-llvm-cov

# generate coverage report to command line by default; otherwise allow
# CI to pass in '--html' (or other formats).

if [ -n "$1" ]; then
  cargo llvm-cov --all-features --workspace "$1"
else
  cargo llvm-cov --all-features --workspace
fi
