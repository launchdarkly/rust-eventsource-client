# eventsource-client

[![Run CI](https://github.com/launchdarkly/rust-eventsource-client/actions/workflows/ci.yml/badge.svg)](https://github.com/launchdarkly/rust-eventsource-client/actions/workflows/ci.yml)

Client for the [Server-Sent Events] protocol (aka [EventSource]).

This library provides a complete SSE protocol implementation with a built-in HTTP transport powered by hyper v1. The pluggable transport design also allows you to use your own HTTP client (reqwest, custom, etc.) if needed.

[Server-Sent Events]: https://html.spec.whatwg.org/multipage/server-sent-events.html
[EventSource]: https://developer.mozilla.org/en-US/docs/Web/API/EventSource

## Quick Start

### 1. Add dependencies

```toml
[dependencies]
eventsource-client = { version = "0.17", features = ["hyper-rustls-native-roots"] }
futures = "0.3"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

**Features:**
- `hyper` - Enables the built-in `HyperTransport` for HTTP support (enabled by default)
- `hyper-rustls-native-roots`, `hyper-rustls-webpki-roots`, or `native-tls` - Adds HTTPS support via rustls (optional)

### 2. Use the client

```rust
use eventsource_client::{ClientBuilder, SSE};
use launchdarkly_sdk_transport::HyperTransport;
use futures::TryStreamExt;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create HTTP transport with timeouts
    let transport = HyperTransport::builder()
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(30))
        .build_https();  // or .build_http() for plain HTTP

    // Build SSE client
    let client = ClientBuilder::for_url("https://example.com/stream")?
        .header("Authorization", "Bearer token")?
        .build_with_transport(transport);

    // Stream events
    let mut stream = client.stream();

    while let Some(event) = stream.try_next().await? {
        match event {
            SSE::Event(evt) => println!("Event: {}", evt.event_type),
            SSE::Comment(c) => println!("Comment: {}", c),
            SSE::Connected(_) => println!("Connected!"),
        }
    }

    Ok(())
}
```

## Example

The `tail` example demonstrates a complete SSE client using the built-in `HyperTransport`:

**Run with HTTP:**
```bash
cargo run --example tail --features hyper -- http://sse.dev/test "Bearer token"
```

**Run with HTTPS:**
```bash
cargo run --example tail --features hyper-rustls-native-roots -- https://sse.dev/test "Bearer token"
cargo run --example tail --features hyper-rustls-webpki-roots -- https://sse.dev/test "Bearer token"
cargo run --example tail --features native-tls -- https://sse.dev/test "Bearer token"
```

The example shows:
- Creating a `HyperTransport` with custom timeouts
- Building an SSE client with authentication headers
- Configuring automatic reconnection with exponential backoff
- Handling different SSE event types (events, comments, connection status)
- Proper error handling for HTTPS URLs without the `hyper-rustls-native-roots` feature

See [`examples/tail.rs`](https://github.com/launchdarkly/rust-eventsource-client/tree/main/eventsource-client/examples/tail.rs) for the complete implementation.

## Features

* **Built-in HTTP transport** - Production-ready `HyperTransport` powered by hyper v1
* **Configurable timeouts** - Connect, read, and write timeout support
* **HTTPS support** - Optional rustls integration via the `hyper-rustls-*` or `native-tls` features
* **Pluggable transport** - Use a custom HTTP client if needed (reqwest, etc.)
* **Tokio-based streaming** - Efficient async/await support
* **Custom headers** - Full control over HTTP requests
* **Automatic reconnection** - Configurable exponential backoff
* **Retry logic** - Handle transient failures gracefully
* **Redirect following** - Automatic handling of HTTP redirects
* **Last-Event-ID** - Resume streams from last received event

## Custom HTTP Transport

While the built-in `HyperTransport` works for most use cases, you can implement the `HttpTransport` trait to use your own HTTP client:

```rust
use launchdarkly_sdk_transport::{HttpTransport, Request, ResponseFuture};
use bytes::Bytes;

#[derive(Clone)]
struct MyTransport {
    // Your HTTP client here
}

impl HttpTransport for MyTransport {
    fn request(&self, request: Request<Option<Bytes>>) -> ResponseFuture {
        // Implement HTTP request handling
        // See the HttpTransport trait documentation for details
        todo!()
    }
}
```

This allows you to:
- Use a different HTTP client (reqwest, custom, etc.)
- Implement custom connection pooling or proxy logic
- Add specialized middleware or observability

## Architecture

```
┌─────────────────────────────────────┐
│   Your Application                  │
└─────────────┬───────────────────────┘
              │
              ▼
┌─────────────────────────────────────┐
│   eventsource-client                │
│   (SSE Protocol Implementation)     │
└─────────────┬───────────────────────┘
              │ HttpTransport trait
              ▼
┌─────────────────────────────────────┐
│   HTTP Transport Layer              │
│   • HyperTransport (built-in)       │
│   • Custom (reqwest, etc.)          │
└─────────────────────────────────────┘
```

## Stability

This library is actively maintained. The SSE protocol implementation is stable. Breaking changes follow semantic versioning.
