use std::{
    fmt::{self, Debug, Display, Formatter},
    future::Future,
    mem,
    pin::Pin,
    str::FromStr,
    task::{Context, Poll},
    time::Duration,
};

use futures::{ready, Stream};
use hyper::{
    body::{Bytes, HttpBody},
    client::{connect::Connect, ResponseFuture},
    header::{HeaderMap, HeaderName, HeaderValue},
    Body, Request, StatusCode, Uri,
};
#[cfg(feature = "rustls")]
use hyper_rustls::HttpsConnector as RustlsConnector;
use log::{debug, info, trace, warn};
use pin_project::pin_project;
use tokio::time::Sleep;

use super::config::ReconnectOptions;
use super::decode::Decoded;
use super::error::{Error, Result};

pub use hyper::client::HttpConnector;
#[cfg(feature = "rustls")]
pub type HttpsConnector = RustlsConnector<HttpConnector>;

/*
 * TODO remove debug output
 * TODO specify list of stati to not retry (e.g. 204)
 */

pub struct ClientBuilder {
    url: Uri,
    headers: HeaderMap,
    reconnect_opts: ReconnectOptions,
}

impl ClientBuilder {
    /// Set a HTTP header on the SSE request.
    pub fn header(mut self, name: &str, value: &str) -> Result<ClientBuilder> {
        let name =
            HeaderName::from_str(name).map_err(|_| Error::HttpRequest(StatusCode::BAD_REQUEST))?;

        let value = HeaderValue::from_str(value)
            .map_err(|_| Error::HttpRequest(StatusCode::BAD_REQUEST))?;

        self.headers.insert(name, value);
        Ok(self)
    }

    /// Configure the client's reconnect behaviour according to the supplied
    /// [`ReconnectOptions`].
    ///
    /// [`ReconnectOptions`]: struct.ReconnectOptions.html
    pub fn reconnect(mut self, opts: ReconnectOptions) -> ClientBuilder {
        self.reconnect_opts = opts;
        self
    }

    pub fn build_with_conn<C>(self, conn: C) -> Client<C>
    where
        C: Connect + Clone,
    {
        Client {
            http: hyper::Client::builder().build(conn),
            request_props: RequestProps {
                url: self.url,
                headers: self.headers,
                reconnect_opts: self.reconnect_opts,
            },
        }
    }

    pub fn build_http(self) -> Client<HttpConnector> {
        self.build_with_conn(HttpConnector::new())
    }

    #[cfg(feature = "rustls")]
    pub fn build(self) -> Client<HttpsConnector> {
        let conn = HttpsConnector::with_native_roots();
        self.build_with_conn(conn)
    }

    pub fn build_with_http_client<C>(self, http: hyper::Client<C>) -> Client<C> {
        Client {
            http,
            request_props: RequestProps {
                url: self.url,
                headers: self.headers,
                reconnect_opts: self.reconnect_opts,
            },
        }
    }
}

#[derive(Clone)]
struct RequestProps {
    url: Uri,
    headers: HeaderMap,
    reconnect_opts: ReconnectOptions,
}

/// Client that connects to a server using the Server-Sent Events protocol
/// and consumes the event stream indefinitely.
pub struct Client<C> {
    http: hyper::Client<C>,
    request_props: RequestProps,
}

impl Client<()> {
    /// Construct a new `Client` (via a [`ClientBuilder`]). This will not
    /// perform any network activity until [`.stream()`] is called.
    ///
    /// [`ClientBuilder`]: struct.ClientBuilder.html
    /// [`.stream()`]: #method.stream
    pub fn for_url(url: &str) -> Result<ClientBuilder> {
        let url = url
            .parse()
            .map_err(|e| Error::InvalidParameter(Box::new(e)))?;
        Ok(ClientBuilder {
            url,
            headers: HeaderMap::new(),
            reconnect_opts: ReconnectOptions::default(),
        })
    }
}

pub type EventStream<C> = Decoded<ReconnectingRequest<C>>;

impl<C> Client<C> {
    /// Connect to the server and begin consuming the stream. Produces a
    /// [`Stream`] of [`Event`](crate::Event)s wrapped in [`Result`].
    ///
    /// Do not use the stream after it returned an error!
    ///
    /// After the first successful connection, the stream will
    /// reconnect for retryable errors.
    pub fn stream(&self) -> EventStream<C>
    where
        C: Connect + Clone + Send + Sync + 'static,
    {
        Decoded::new(ReconnectingRequest::new(
            self.http.clone(),
            self.request_props.clone(),
        ))
    }
}

#[must_use = "streams do nothing unless polled"]
#[pin_project]
pub struct ReconnectingRequest<C> {
    http: hyper::Client<C>,
    props: RequestProps,
    #[pin]
    state: State,
    next_reconnect_delay: Duration,
}

#[allow(clippy::large_enum_variant)] // false positive
#[pin_project(project = StateProj)]
enum State {
    New,
    Connecting {
        retry: bool,
        #[pin]
        resp: ResponseFuture,
    },
    Connected(#[pin] hyper::Body),
    WaitingToReconnect(#[pin] Sleep),
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

impl<C> ReconnectingRequest<C> {
    fn new(http: hyper::Client<C>, props: RequestProps) -> ReconnectingRequest<C> {
        let reconnect_delay = props.reconnect_opts.delay;
        ReconnectingRequest {
            props,
            http,
            state: State::New,
            next_reconnect_delay: reconnect_delay,
        }
    }
}

impl<C> ReconnectingRequest<C> {
    fn send_request(&self) -> Result<ResponseFuture>
    where
        C: Connect + Clone + Send + Sync + 'static,
    {
        let mut request = Request::get(&self.props.url);
        *request.headers_mut().unwrap() = self.props.headers.clone();
        let request = request
            .body(Body::empty())
            .map_err(|e| Error::InvalidParameter(Box::new(e)))?;
        Ok(self.http.request(request))
    }

    fn backoff(mut self: Pin<&mut Self>) -> Duration {
        let delay = self.next_reconnect_delay;
        let this = self.as_mut().project();
        let mut next_reconnect_delay = std::cmp::min(
            this.props.reconnect_opts.delay_max,
            *this.next_reconnect_delay * this.props.reconnect_opts.backoff_factor,
        );
        mem::swap(this.next_reconnect_delay, &mut next_reconnect_delay);
        delay
    }

    fn reset_backoff(self: Pin<&mut Self>) {
        let mut delay = self.props.reconnect_opts.delay;
        let this = self.project();
        mem::swap(this.next_reconnect_delay, &mut delay);
    }
}

fn delay(dur: Duration, description: &str) -> Sleep {
    info!("Waiting {:?} before {}", dur, description);
    tokio::time::sleep(dur)
}

#[derive(Debug)]
struct StatusError {
    status: StatusCode,
}

impl Display for StatusError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "Invalid status code: {}", self.status)
    }
}

impl std::error::Error for StatusError {}

impl<C> Stream for ReconnectingRequest<C>
where
    C: Connect + Clone + Send + Sync + 'static,
{
    type Item = Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        trace!("ReconnectingRequest::poll({:?})", &self.state);

        loop {
            trace!("ReconnectingRequest::poll loop({:?})", &self.state);

            let this = self.as_mut().project();
            let state = this.state.project();

            match state {
                // New immediately transitions to Connecting, and exists only
                // to ensure that we only connect when polled.
                StateProj::New => {
                    let resp = match self.send_request() {
                        Err(e) => return Poll::Ready(Some(Err(e))),
                        Ok(r) => r,
                    };
                    let retry = self.props.reconnect_opts.retry_initial;
                    self.as_mut()
                        .project()
                        .state
                        .set(State::Connecting { resp, retry })
                }
                StateProj::Connecting { retry, resp } => match ready!(resp.poll(cx)) {
                    Ok(resp) => {
                        debug!("HTTP response: {:#?}", resp);

                        if !resp.status().is_success() {
                            self.as_mut().project().state.set(State::New);
                            return Poll::Ready(Some(Err(Error::HttpRequest(resp.status()))));
                        }

                        self.as_mut().reset_backoff();
                        self.as_mut()
                            .project()
                            .state
                            .set(State::Connected(resp.into_body()))
                    }
                    Err(e) => {
                        warn!("request returned an error: {}", e);
                        if !*retry {
                            self.as_mut().project().state.set(State::New);
                            return Poll::Ready(Some(Err(Error::HttpStream(Box::new(e)))));
                        }

                        let duration = self.as_mut().backoff();
                        self.as_mut()
                            .project()
                            .state
                            .set(State::WaitingToReconnect(delay(duration, "retrying")))
                    }
                },
                StateProj::Connected(body) => match ready!(body.poll_data(cx)) {
                    Some(Ok(result)) => {
                        return Poll::Ready(Some(Ok(result)));
                    }
                    res => {
                        // reconnect
                        if self.props.reconnect_opts.reconnect {
                            let duration = self.as_mut().backoff();
                            self.as_mut()
                                .project()
                                .state
                                .set(State::WaitingToReconnect(delay(duration, "reconnecting")))
                        } else {
                            return Poll::Ready(
                                res.map(|r| r.map_err(|e| Error::HttpStream(Box::new(e)))),
                            );
                        }
                    }
                },
                StateProj::WaitingToReconnect(delay) => {
                    ready!(delay.poll(cx));
                    info!("Reconnecting");
                    let resp = self.send_request()?;
                    self.as_mut()
                        .project()
                        .state
                        .set(State::Connecting { retry: true, resp })
                }
            };
        }
    }
}
