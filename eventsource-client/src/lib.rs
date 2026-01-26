#![warn(rust_2018_idioms)]
#![allow(clippy::result_large_err)]
//! Client for the [Server-Sent Events] protocol (aka [EventSource]).
//!
//! ```
//! use futures::{TryStreamExt};
//! # use eventsource_client::Error;
//! use eventsource_client::{Client, SSE};
//! # #[tokio::main]
//! # async fn main() -> Result<(), eventsource_client::Error> {
//! let mut client = eventsource_client::ClientBuilder::for_url("https://example.com/stream")?
//!     .header("Authorization", "Basic username:password")?
//!     .build();
//!
//! let mut stream = Box::pin(client.stream())
//!     .map_ok(|event| match event {
//!         SSE::Comment(comment) => println!("got a comment event: {:?}", comment),
//!         SSE::Event(evt) => println!("got an event: {}", evt.event_type),
//!         SSE::Connected(_) => println!("got connected")
//!     })
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
mod error;
mod event_parser;
mod response;
mod retry;

pub use client::*;
pub use config::*;
pub use error::*;
pub use event_parser::Event;
pub use event_parser::SSE;
pub use response::Response;
