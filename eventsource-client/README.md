# eventsource-client

[![Run CI](https://github.com/launchdarkly/rust-eventsource-client/actions/workflows/ci.yml/badge.svg)](https://github.com/launchdarkly/rust-eventsource-client/actions/workflows/ci.yml)

Client for the [Server-Sent Events] protocol (aka [EventSource]).

This library focuses on the SSE protocol implementation. You provide the HTTP transport layer (hyper, reqwest, etc.), giving you full control over HTTP configuration like timeouts, TLS, and connection pooling.

[Server-Sent Events]: https://html.spec.whatwg.org/multipage/server-sent-events.html
[EventSource]: https://developer.mozilla.org/en-US/docs/Web/API/EventSource

## Requirements

* Tokio async runtime
* An HTTP client library (hyper, reqwest, or custom)

## Quick Start

### 1. Add dependencies

```toml
[dependencies]
eventsource-client = "0.17"
reqwest = { version = "0.12", features = ["stream"] }  # or hyper v1
futures = "0.3"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

### 2. Implement HttpTransport

Use one of our example implementations:

```rust
// See examples/reqwest_transport.rs for complete implementation
use eventsource_client::{HttpTransport, ResponseFuture};

struct ReqwestTransport {
    client: reqwest::Client,
}

impl HttpTransport for ReqwestTransport {
    fn request(&self, request: http::Request<()>) -> ResponseFuture {
        // Convert request and call HTTP client
        // See examples/ for full implementation
    }
}
```

### 3. Use the client

```rust
use eventsource_client::{ClientBuilder, SSE};
use futures::TryStreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create HTTP transport
    let transport = ReqwestTransport::new()?;

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

## Features

* **Pluggable HTTP transport** - Use any HTTP client (hyper, reqwest, or custom)
* **Tokio-based streaming** - Efficient async/await support
* **Custom headers** - Full control over HTTP requests
* **Automatic reconnection** - Configurable exponential backoff
* **Retry logic** - Handle transient failures gracefully
* **Redirect following** - Automatic handling of HTTP redirects
* **Last-Event-ID** - Resume streams from last received event

## Migration from v0.16

If you're upgrading from v0.16 (which used hyper 0.14 internally), see [MIGRATION.md](MIGRATION.md) for a detailed migration guide.

Key changes:
- You must now provide an HTTP transport implementation
- Removed `build()`, `build_http()`, and other hyper-specific methods
- Use `build_with_transport(transport)` instead
- Timeout configuration moved to your HTTP transport

## Why Pluggable Transport?

1. **Use latest HTTP clients** - Not locked to a specific HTTP library version
2. **Full control** - Configure timeouts, TLS, proxies, etc. exactly as needed
3. **Smaller library** - Focused on SSE protocol, not HTTP implementation
4. **Flexibility** - Swap HTTP clients without changing SSE code

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
│   Your HTTP Client                  │
│   (hyper, reqwest, custom, etc.)    │
└─────────────────────────────────────┘
```

## Stability

Early stage release for feedback purposes. May contain bugs or performance
issues. API subject to change.
