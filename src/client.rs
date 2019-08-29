use std::fmt::{self, Debug, Formatter};

use futures::future::Future;
use futures::stream::{MapErr, Stream};
use futures::{Async, Poll};
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
            request_props: RequestProps {
                url: self.url,
                headers: self.headers,
            },
        }
    }
}

#[derive(Clone)]
struct RequestProps {
    url: r::Url,
    headers: r::header::HeaderMap,
}

/// Client that connects to a server using the Server-Sent Events protocol
/// and consumes the event stream indefinitely.
pub struct Client {
    request_props: RequestProps,
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
        Box::new(Decoded::new(ReconnectingRequest::new(
            self.request_props.clone(),
        )))
    }
}

#[must_use = "streams do nothing unless polled"]
struct ReconnectingRequest {
    props: RequestProps,
    http: ra::Client,
    state: State,
}

enum State {
    New,
    // TODO remove box somehow
    Connecting(Box<dyn Future<Item = ra::Response, Error = Error> + Send>),
    Connected(MapErr<ra::Decoder, fn(r::Error) -> Error>),
    // TODO actually reconnect
}

impl State {
    fn name(&self) -> &'static str {
        match self {
            State::New => "new",
            State::Connecting(_) => "connecting",
            State::Connected(_) => "connected",
        }
    }
}

impl Debug for State {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

impl ReconnectingRequest {
    fn new(props: RequestProps) -> ReconnectingRequest {
        let http = ra::Client::new();
        ReconnectingRequest {
            props,
            http,
            state: State::New,
        }
    }
}

impl Stream for ReconnectingRequest {
    type Item = ra::Chunk;
    type Error = Error;

    fn poll(&mut self) -> Poll<Option<ra::Chunk>, Error> {
        trace!("ReconnectingRequest::poll({:?})", &self.state);

        loop {
            let new_state = match self.state {
                State::New => {
                    let request = self
                        .http
                        .get(self.props.url.clone())
                        .headers(self.props.headers.clone());
                    let resp = request.send().map_err(|e| Error::HttpRequest(Box::new(e)));
                    State::Connecting(Box::new(resp))
                }
                State::Connecting(ref mut resp) => {
                    let resp = try_ready!(resp.poll());
                    debug!("HTTP response: {:#?}", resp);

                    match resp.error_for_status() {
                        Ok(resp) => State::Connected(
                            resp.into_body().map_err(|e| Error::HttpStream(Box::new(e))),
                        ),
                        Err(e) => return Err(Error::HttpRequest(Box::new(e))),
                    }
                }
                State::Connected(ref mut chunks) => {
                    // TODO no, handle err directly
                    match try_ready!(chunks.poll()) {
                        Some(c) => return Ok(Async::Ready(Some(c))),
                        None => return Ok(Async::Ready(None)),
                    }
                }
            };
            self.state = new_state;
        }
    }
}
