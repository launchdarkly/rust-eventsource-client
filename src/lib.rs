//! Client for the [Server-Sent Events] protocol (aka [EventSource]).
//!
//! ```
//! use eventsource_client::Client;
//! # use futures::{lazy, future::Future, stream::Stream};
//!
//! # fn main() -> Result<(), eventsource_client::Error> {
//! let mut client = Client::for_url("https://example.com/stream")?
//!     .header("Authorization", "Basic username:password")?
//!     .build();
//!
//! # tokio::run(lazy(move || {
//! client.stream()
//!     .for_each(|event| {
//!         Ok(println!("got an event: {}", event.event_type))
//!     })
//!     .map_err(|e| println!("error streaming events: {:?}", e));
//! # Ok(())
//! # }));
//! # Ok(())
//! # }
//! ```
//!
//![Server-Sent Events]: https://html.spec.whatwg.org/multipage/server-sent-events.html
//![EventSource]: https://developer.mozilla.org/en-US/docs/Web/API/EventSource

mod client;
mod config;
mod decode;
mod error;

pub use client::*;
pub use config::*;
pub use decode::{Event, EventStream};
pub use error::*;
