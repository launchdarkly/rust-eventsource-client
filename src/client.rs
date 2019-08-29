use std::fmt::{self, Debug, Formatter};
use std::time::{Duration, Instant};

use futures::future::Future;
use futures::stream::{MapErr, Stream};
use futures::{Async, Poll};
use reqwest as r;
use reqwest::{r#async as ra, Url};
use tokio::timer::Delay;

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
    Connecting {
        retry: bool,
        // TODO remove box somehow
        resp: Box<dyn Future<Item = ra::Response, Error = Error> + Send>,
    },
    Connected(MapErr<ra::Decoder, fn(r::Error) -> Error>),
    WaitingToReconnect(Delay),
}

impl State {
    fn name(&self) -> &'static str {
        match self {
            State::New => "new",
            State::Connecting { retry: false, .. } => "connecting(no-retry)",
            State::Connecting { retry: true, .. } => "connecting(retry)",
            State::Connected(_) => "connected",
            State::WaitingToReconnect(_) => "waiting-to-reconnect",
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

impl ReconnectingRequest {
    fn send_request(&self) -> impl Future<Item = ra::Response, Error = Error> + Send {
        let request = self
            .http
            .get(self.props.url.clone())
            .headers(self.props.headers.clone());
        request.send().map_err(|e| Error::HttpRequest(Box::new(e)))
    }
}

impl Stream for ReconnectingRequest {
    type Item = ra::Chunk;
    type Error = Error;

    fn poll(&mut self) -> Poll<Option<ra::Chunk>, Error> {
        trace!("ReconnectingRequest::poll({:?})", &self.state);

        loop {
            trace!("ReconnectingRequest::poll loop({:?})", &self.state);
            let new_state = match self.state {
                // New immediately transitions to Connecting, and exists only
                // to ensure that we only connect when polled.
                State::New => {
                    let resp = self.send_request();
                    State::Connecting {
                        retry: false,
                        resp: Box::new(resp),
                    }
                }
                State::Connecting {
                    retry,
                    ref mut resp,
                } => {
                    let resp = match resp.poll() {
                        Ok(Async::Ready(resp)) => resp,
                        Ok(Async::NotReady) => return Ok(Async::NotReady),
                        Err(e) => {
                            warn!("request returned an error: {:?}", e);
                            if retry {
                                self.state = State::WaitingToReconnect(Delay::new(
                                    Instant::now() + Duration::from_secs(1),
                                ));
                                continue;
                            } else {
                                return Err(e);
                            }
                        }
                    };
                    debug!("HTTP response: {:#?}", resp);

                    match resp.error_for_status() {
                        Ok(resp) => State::Connected(
                            resp.into_body().map_err(|e| Error::HttpStream(Box::new(e))),
                        ),
                        Err(e) => return Err(Error::HttpRequest(Box::new(e))),
                    }
                }
                State::Connected(ref mut chunks) => match chunks.poll() {
                    Ok(result) => return Ok(result),
                    Err(e) => {
                        warn!("chunk stream returned an error: {:?}", e);
                        State::WaitingToReconnect(Delay::new(
                            Instant::now() + Duration::from_secs(1),
                        ))
                    }
                },
                State::WaitingToReconnect(ref mut delay) => {
                    try_ready!(delay.poll().map_err(|_| panic!("TODO")));
                    // TODO backoff
                    info!("Reconnecting");
                    let resp = self.send_request();
                    State::Connecting {
                        retry: true,
                        resp: Box::new(resp),
                    }
                }
            };
            self.state = new_state;
        }
    }
}
