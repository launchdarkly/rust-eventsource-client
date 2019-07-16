//! Client for the Server-Sent Events protocol (aka eventsource).
//!
//! ```
//! use eventsource_client::Client;
//! # fn main() -> Result<(), eventsource_client::Error> {
//! let mut client = Client::for_url("http://example.com/stream")?
//!     .header("Authorization", "Basic username:password")?
//!     .build();
//! let events = client.stream();
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
