//! Client for the Server-Sent Events protocol (aka eventsource).
//!
//! ```
//! use eventsource_client::Client;
//! # use futures::{lazy, future::Future, stream::Stream};
//! # fn main() -> Result<(), eventsource_client::Error> {
//! let mut client = Client::for_url("https://example.com/stream")?
//!     .header("Authorization", "Basic username:password")?
//!     .build();
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

#[macro_use]
extern crate futures;

#[macro_use]
extern crate log;

#[cfg(test)]
#[macro_use]
extern crate maplit;

mod client;

pub use client::*;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
