# Change log

All notable changes to the project will be documented in this file. This project adheres to [Semantic Versioning](http://semver.org).

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
