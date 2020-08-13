# Guide to developing rust-eventsource-client

Incomplete.

## Get detailed logging

eventsource-client uses the standard [log crate](https://crates.io/crates/log) for logging. It will log additional detail about the protocol implementation at `trace` level.

e.g. if using [env_logger](https://crates.io/crates/env_logger) (as the example script does), set `RUST_LOG=eventsource_client=trace`.
