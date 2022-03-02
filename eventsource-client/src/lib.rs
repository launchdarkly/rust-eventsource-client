//! Client for the [Server-Sent Events] protocol (aka [EventSource]).
//!
//! ```
//! use futures::{Stream, TryStreamExt};
//! use eventsource_client as es;
//! use es::{Client, Error};
//! # #[tokio::main]
//! # async fn main() -> Result<(), Error> {
//! let mut client = es::ClientBuilder::for_url("https://example.com/stream")?
//!     .header("Authorization", "Basic username:password")?
//!     .build();
//!
//! let mut stream = Box::pin(client.stream())
//!     .map_ok(|event| println!("got an event: {}", event.event_type))
//!     .map_err(|e| println!("error streaming events: {:?}", e));
//! # while let Ok(Some(_)) = stream.try_next().await {}
//! #
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
pub use decode::Event;
pub use error::*;
