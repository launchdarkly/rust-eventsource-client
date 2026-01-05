#![warn(rust_2018_idioms)]
//! Client for the [Server-Sent Events] protocol (aka [EventSource]).
//!
//! This library provides SSE protocol support but requires you to bring your own
//! HTTP transport. See the `examples/` directory for reference implementations using
//! popular HTTP clients like hyper and reqwest.
//!
//! # Getting Started
//!
//! ```ignore
//! use futures::TryStreamExt;
//! use eventsource_client::{Client, ClientBuilder, SSE};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // You need to implement HttpTransport trait for your HTTP client
//! // See examples/hyper_transport.rs or examples/reqwest_transport.rs for reference implementations
//! # struct MyTransport;
//! # impl eventsource_client::HttpTransport for MyTransport {
//! #     fn request(&self, _req: http::Request<Option<String>>) -> eventsource_client::ResponseFuture {
//! #         unimplemented!()
//! #     }
//! # }
//! let transport = MyTransport::new();
//!
//! let client = ClientBuilder::for_url("https://example.com/stream")?
//!     .header("Authorization", "Bearer token")?
//!     .build_with_transport(transport);
//!
//! let mut stream = client.stream();
//!
//! while let Some(event) = stream.try_next().await? {
//!     match event {
//!         SSE::Event(evt) => println!("Event: {}", evt.event_type),
//!         SSE::Comment(comment) => println!("Comment: {}", comment),
//!         SSE::Connected(_) => println!("Connected!"),
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Implementing a Transport
//!
//! See the [`transport`] module documentation for details on implementing
//! the [`HttpTransport`] trait.
//!
//! [`HttpTransport`]: HttpTransport
//!
//! [Server-Sent Events]: https://html.spec.whatwg.org/multipage/server-sent-events.html
//! [EventSource]: https://developer.mozilla.org/en-US/docs/Web/API/EventSource

mod client;
mod config;
mod error;
mod event_parser;
mod response;
mod retry;
mod transport;
#[cfg(feature = "hyper")]
mod transport_hyper;

pub use client::*;
pub use config::*;
pub use error::*;
pub use event_parser::Event;
pub use event_parser::SSE;
pub use response::Response;
pub use transport::*;
#[cfg(feature = "hyper")]
pub use transport_hyper::*;
