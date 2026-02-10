# Change log

All notable changes to the project will be documented in this file. This project adheres to [Semantic Versioning](http://semver.org).

## [0.16.1](https://github.com/launchdarkly/rust-eventsource-client/compare/0.16.0...0.16.1) (2026-01-26)


### Bug Fixes

* Bump MSRV from 1.83 to 1.88 ([#110](https://github.com/launchdarkly/rust-eventsource-client/issues/110)) ([94cdd71](https://github.com/launchdarkly/rust-eventsource-client/commit/94cdd716bbcf0fc552b7a470142ccad9149848aa))
* Bump MSRV from 1.88 to 1.91 ([#112](https://github.com/launchdarkly/rust-eventsource-client/issues/112)) ([78423ab](https://github.com/launchdarkly/rust-eventsource-client/commit/78423abfa406550d9270fba33aebc25e257b0216))

## [0.16.0](https://github.com/launchdarkly/rust-eventsource-client/compare/0.15.1...0.16.0) (2025-12-17)


### ⚠ BREAKING CHANGES

* Stream will return `Poll::Ready(None)` when client has closed stream
* Removed `Error::StreamClosed` in lieu of returning `Poll::Ready(None)`

### Bug Fixes

* Bump MSRV to 1.83 ([230d00c](https://github.com/launchdarkly/rust-eventsource-client/commit/230d00c95c6d49c8f49d5e1f5fe92e1d12599ce6))
* Removed `Error::StreamClosed` in lieu of returning `Poll::Ready(None)` ([230d00c](https://github.com/launchdarkly/rust-eventsource-client/commit/230d00c95c6d49c8f49d5e1f5fe92e1d12599ce6))
* Stop connecting if stream fails on initial request without opting into retry ([230d00c](https://github.com/launchdarkly/rust-eventsource-client/commit/230d00c95c6d49c8f49d5e1f5fe92e1d12599ce6))
* Stream will return `Poll::Ready(None)` when client has closed stream ([230d00c](https://github.com/launchdarkly/rust-eventsource-client/commit/230d00c95c6d49c8f49d5e1f5fe92e1d12599ce6))

## [0.15.1](https://github.com/launchdarkly/rust-eventsource-client/compare/0.15.0...0.15.1) (2025-10-07)


### Bug Fixes

* Bump MSRV to 1.82.0 ([#95](https://github.com/launchdarkly/rust-eventsource-client/issues/95)) ([ddf8753](https://github.com/launchdarkly/rust-eventsource-client/commit/ddf8753b8bebcb2e8c4f55b8ba081adbb214825c))
* Reconnect should use both reconnect and retry_initial for reconnection ([#99](https://github.com/launchdarkly/rust-eventsource-client/issues/99)) ([9a91e67](https://github.com/launchdarkly/rust-eventsource-client/commit/9a91e67c62baaac0660b114df6429b79f9308b0c))

## [0.15.0](https://github.com/launchdarkly/rust-eventsource-client/compare/0.14.0...0.15.0) (2025-03-27)


### Features

* Add connect and write timeout configurations options ([#93](https://github.com/launchdarkly/rust-eventsource-client/issues/93)) ([cfb9321](https://github.com/launchdarkly/rust-eventsource-client/commit/cfb9321633f71f95116a61fe4d7a14c0aca4f982))


### Bug Fixes

* Bump MSRV to 1.81 ([#91](https://github.com/launchdarkly/rust-eventsource-client/issues/91)) ([3b63d64](https://github.com/launchdarkly/rust-eventsource-client/commit/3b63d644b1d9475ed7ec69bb3f386badb77abc63))

## [0.14.0](https://github.com/launchdarkly/rust-eventsource-client/compare/0.13.0...0.14.0) (2025-02-24)


### Features

* Add `basic_auth` method to `ClientBuilder` ([#88](https://github.com/launchdarkly/rust-eventsource-client/issues/88)) ([526b03f](https://github.com/launchdarkly/rust-eventsource-client/commit/526b03ffeb5b0ce7f42c531b1b579e2f9a4e9662))

## [0.13.0](https://github.com/launchdarkly/rust-eventsource-client/compare/0.12.2...0.13.0) (2024-07-30)


### Features

* Emit `SSE::Connected` event when stream is established ([#79](https://github.com/launchdarkly/rust-eventsource-client/issues/79)) ([791faf4](https://github.com/launchdarkly/rust-eventsource-client/commit/791faf4f2cda2165cf9df50a181344979d43429c))
* Update `Error::UnexpectedResponse` to include failed connection details ([#79](https://github.com/launchdarkly/rust-eventsource-client/issues/79)) ([791faf4](https://github.com/launchdarkly/rust-eventsource-client/commit/791faf4f2cda2165cf9df50a181344979d43429c))

## [0.12.2](https://github.com/launchdarkly/rust-eventsource-client/compare/0.12.1...0.12.2) (2023-12-20)


### Bug Fixes

* **deps:** Bump hyper to fix CVE-2022-31394 ([#72](https://github.com/launchdarkly/rust-eventsource-client/issues/72)) ([48d9555](https://github.com/launchdarkly/rust-eventsource-client/commit/48d955541dc29695a81b2535dafd7dec2fdb59d8))

## [0.12.1](https://github.com/launchdarkly/rust-eventsource-client/compare/0.12.0...0.12.1) (2023-12-12)


### Bug Fixes

* logify could panic if truncating mid-code point ([#70](https://github.com/launchdarkly/rust-eventsource-client/issues/70)) ([37316c4](https://github.com/launchdarkly/rust-eventsource-client/commit/37316c4f0e8c015db118dc1d082281838e88e522))

## [0.12.0](https://github.com/launchdarkly/rust-eventsource-client/compare/0.11.0...0.12.0) (2023-11-15)


### ⚠ BREAKING CHANGES

* Remove re-export of hyper_rustls types ([#59](https://github.com/launchdarkly/rust-eventsource-client/issues/59))
* Bump dependencies ([#58](https://github.com/launchdarkly/rust-eventsource-client/issues/58))

### deps

* Bump dependencies ([#58](https://github.com/launchdarkly/rust-eventsource-client/issues/58)) ([a7174e3](https://github.com/launchdarkly/rust-eventsource-client/commit/a7174e328f168af0a96f8c9671453a29c028d0f0))


### Features

* make Error implement std::fmt::Display, std::error::Error` and Sync  ([#47](https://github.com/launchdarkly/rust-eventsource-client/issues/47)) ([0eaab6e](https://github.com/launchdarkly/rust-eventsource-client/commit/0eaab6eefb8d69aac01ded4ab53c527c84084ba6))


### Bug Fixes

* Remove re-export of hyper_rustls types ([#59](https://github.com/launchdarkly/rust-eventsource-client/issues/59)) ([ec24970](https://github.com/launchdarkly/rust-eventsource-client/commit/ec24970d4a9ed875a44fb9c84c67b587d46ca23d))

## [0.11.0] - 2022-11-07
### Fixed:
- Add missing retry interval reset behavior.
- Add missing jitter to retry strategy.

## [0.10.2] - 2022-10-28
### Fixed:
- Correctly handle comment payloads.

## [0.10.1] - 2022-04-14
### Fixed:
- Comment events were incorrectly consuming non-comment event data. Now comment events are emitted as they are parsed and can no longer affect non-comment event data.

## [0.10.0] - 2022-03-23
### Added:
- Added support for following 301 & 307 redirects with configurable redirect limit.

### Fixed:
- Fixed `Last-Event-ID` handling when server sends explicit empty ID.

## [0.9.0] - 2022-03-15
### Added:
- Added support for SSE test harness.

### Changed:
- Change `ClientBuilder` to return `impl Client`, where `Client` is a sealed trait that exposes a `stream()` method. 

### Fixed:
- Fixed various bugs related to SSE protocol.

## [0.8.2] - 2022-02-03
### Added:
- Support for creating an event source client with a pre-configured connection.

## [0.8.1] - 2022-01-19
### Changed:
- Added missing changelog
- Fixed keyword for crates.io publishing

## [0.8.0] - 2022-01-19
### Changed:
- Introduced new Error variant
