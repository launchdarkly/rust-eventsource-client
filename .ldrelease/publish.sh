#!/bin/bash

set -ue

export CARGO_REGISTRY_TOKEN="$(cat "${LD_RELEASE_SECRETS_DIR}/rust_cratesio_api_token")"

cd eventsource-client
cargo publish
