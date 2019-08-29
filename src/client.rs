use futures::future::{self, Future};
use futures::stream::Stream;
use reqwest as r;
use reqwest::{r#async as ra, Url};

use super::decode::{Decoded, EventStream};
use super::error::{Error, Result};

/*
 * TODO remove debug output
 * TODO reconnect
 */

pub struct ClientBuilder {
    url: r::Url,
    headers: r::header::HeaderMap,
}

impl ClientBuilder {
    /// Set a HTTP header on the SSE request.
    pub fn header(mut self, key: &'static str, value: &str) -> Result<ClientBuilder> {
        let value = value.parse().map_err(|e| Error::HttpRequest(Box::new(e)))?;
        self.headers.insert(key, value);
        Ok(self)
    }

    pub fn build(self) -> Client {
        Client {
            url: self.url,
            headers: self.headers,
        }
    }
}

/// Client that connects to a server using the Server-Sent Events protocol
/// and consumes the event stream indefinitely.
pub struct Client {
    url: r::Url,
    headers: r::header::HeaderMap,
}

impl Client {
    /// Construct a new `Client` (via a [`ClientBuilder`]). This will not
    /// perform any network activity until [`.stream()`] is called.
    ///
    /// [`ClientBuilder`]: struct.ClientBuilder.html
    /// [`.stream()`]: #method.stream
    pub fn for_url(url: &str) -> Result<ClientBuilder> {
        let url = Url::parse(url).map_err(|e| Error::HttpRequest(Box::new(e)))?;
        Ok(ClientBuilder {
            url: url,
            headers: r::header::HeaderMap::new(),
        })
    }

    /// Connect to the server and begin consuming the stream. Produces a
    /// [`Stream`] of [`Event`]s.
    ///
    /// [`Stream`]: ../futures/stream/trait.Stream.html
    /// [`Event`]: struct.Event.html
    pub fn stream(&mut self) -> EventStream {
        let http = ra::Client::new();
        let request = http.get(self.url.clone()).headers(self.headers.clone());
        let resp = request.send();

        let fut_stream_chunks = resp
            .map_err(|e| Error::HttpRequest(Box::new(e)))
            .and_then(|resp| {
                debug!("HTTP response: {:#?}", resp);

                match resp.error_for_status() {
                    Ok(resp) => {
                        future::ok(resp.into_body().map_err(|e| Error::HttpStream(Box::new(e))))
                    }
                    Err(e) => future::err(Error::HttpRequest(Box::new(e))),
                }
            })
            .flatten_stream();

        Box::new(Decoded::new(fut_stream_chunks))
    }
}
