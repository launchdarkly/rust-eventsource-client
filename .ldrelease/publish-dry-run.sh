#!/bin/bash

set -ue

echo "DRY RUN: not publishing to crates.io, only copying crate"
cp ./target/package/*.crate "${LD_RELEASE_ARTIFACTS_DIR}"
