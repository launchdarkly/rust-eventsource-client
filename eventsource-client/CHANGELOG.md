# Change log

All notable changes to the project will be documented in this file. This project adheres to [Semantic Versioning](http://semver.org).

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
