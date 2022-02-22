# eventsource-client

Client for the [Server-Sent Events] protocol (aka [EventSource]).

[Server-Sent Events]: https://html.spec.whatwg.org/multipage/server-sent-events.html
[EventSource]: https://developer.mozilla.org/en-US/docs/Web/API/EventSource

## Requirements

Requires tokio.

## Usage

Example that just prints the type of each event received:

```rust
use eventsource_client::Client;

let mut client = Client::for_url("https://example.com/stream")?
    .header("Authorization", "Basic username:password")?
    .build();

client.stream()
    .for_each(|event| {
      Ok(println!("got an event: {}", event.event_type))
    })
    .map_err(|e| println!("error streaming events: {:?}", e));
```

(Some boilerplate omitted for clarity; see [examples directory] for complete,
working code.)

[examples directory]: https://github.com/launchdarkly/rust-eventsource-client/tree/master/examples

## Features

* tokio-based streaming client.
* Supports setting custom headers on the HTTP request (e.g. for endpoints
  requiring authorization).
* Retry for failed connections.
* Reconnection if connection is interrupted, with exponential backoff.

## Stability

Early stage release for feedback purposes. May contain bugs or performance
issues. API subject to change.
