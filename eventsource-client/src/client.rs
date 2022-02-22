use std::error::Error as StdError;
use std::str::FromStr;
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
    client::{
        connect::{Connect, Connection},
        ResponseFuture,
    },
    header::{HeaderMap, HeaderName, HeaderValue},
    service::Service,
    Body, Request, StatusCode, Uri,
};
#[cfg(feature = "rustls")]
use hyper_rustls::HttpsConnector as RustlsConnector;
pub use hyper_timeout::TimeoutConnector;
use log::{debug, info, trace, warn};
use pin_project::pin_project;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::Sleep,
};

use crate::event_parser::EventParser;
use crate::event_parser::SSE;

use super::config::ReconnectOptions;
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
    method: String,
    body: Option<String>,
    reconnect_opts: ReconnectOptions,
    read_timeout: Option<Duration>,
    last_event_id: String,
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

    /// Set the request method used for the initial connection to the SSE endpoint.
    pub fn method(mut self, method: String) -> ClientBuilder {
        self.method = method;
        self
    }

    /// Set the request body used for the initial connection to the SSE endpoint.
    pub fn body(mut self, body: String) -> ClientBuilder {
        self.body = Some(body);
        self
    }

    /// Set the last event id for a stream when it is created. If it is set, it will be sent to the
    /// server in case it can replay missed events.
    pub fn last_event_id(mut self, last_event_id: String) -> ClientBuilder {
        self.last_event_id = last_event_id;
        self
    }

    /// Set a read timeout for the underlying connection. There is no read timeout by default.
    pub fn read_timeout(mut self, read_timeout: Duration) -> ClientBuilder {
        self.read_timeout = Some(read_timeout);
        self
    }

    /// Configure the client's reconnect behaviour according to the supplied
    /// [`ReconnectOptions`].
    ///
    /// [`ReconnectOptions`]: struct.ReconnectOptions.html
    pub fn reconnect(mut self, opts: ReconnectOptions) -> ClientBuilder {
        self.reconnect_opts = opts;
        self
    }

    fn build_with_conn<C>(self, conn: C) -> Client<TimeoutConnector<C>>
    where
        C: Service<Uri> + Send + 'static + std::clone::Clone,
        C::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
        C::Future: Unpin + Send,
        C::Response: AsyncRead + AsyncWrite + Connection + Unpin + Send + 'static,
    {
        let mut connector = TimeoutConnector::new(conn);
        connector.set_read_timeout(self.read_timeout);
        let client = hyper::Client::builder().build::<_, hyper::Body>(connector);

        Client {
            http: client,
            request_props: RequestProps {
                url: self.url,
                headers: self.headers,
                reconnect_opts: self.reconnect_opts,
                method: self.method,
                body: self.body,
            },
            last_event_id: self.last_event_id,
        }
    }

    pub fn build_http(self) -> Client<TimeoutConnector<HttpConnector>> {
        self.build_with_conn(HttpConnector::new())
    }

    #[cfg(feature = "rustls")]
    pub fn build(self) -> Client<TimeoutConnector<HttpsConnector>> {
        let conn = HttpsConnector::with_native_roots();
        self.build_with_conn(conn)
    }

    pub fn build_with_http_client<C>(self, http: hyper::Client<C>) -> Client<C> {
        Client {
            http,
            request_props: RequestProps {
                url: self.url,
                headers: self.headers,
                method: self.method,
                body: self.body,
                reconnect_opts: self.reconnect_opts,
            },
            last_event_id: self.last_event_id,
        }
    }
}

#[derive(Clone)]
struct RequestProps {
    url: Uri,
    headers: HeaderMap,
    method: String,
    body: Option<String>,
    reconnect_opts: ReconnectOptions,
}

/// Client that connects to a server using the Server-Sent Events protocol
/// and consumes the event stream indefinitely.
pub struct Client<C> {
    http: hyper::Client<C>,
    request_props: RequestProps,
    last_event_id: String,
}

impl Client<()> {
    /// Construct a new `Client` (via a [`ClientBuilder`]). This will not
    /// perform any network activity until [`.stream()`] is called.
    ///
    /// [`ClientBuilder`]: struct.ClientBuilder.html
    /// [`.stream()`]: #method.stream
    pub fn for_url(url: &str) -> Result<ClientBuilder> {
        let url = url.parse().map_err(|e| Error::HttpRequest(Box::new(e)))?;

        let mut header_map = HeaderMap::new();
        header_map.insert("Accept", HeaderValue::from_static("text/event-stream"));
        header_map.insert("Cache-Control", HeaderValue::from_static("no-cache"));

        Ok(ClientBuilder {
            url,
            method: String::from("GET"),
            headers: header_map,
            body: None,
            reconnect_opts: ReconnectOptions::default(),
            last_event_id: String::new(),
            read_timeout: None,
        })
    }
}

pub type EventStream<C> = ReconnectingRequest<C>;

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
        ReconnectingRequest::new(
            self.http.clone(),
            self.request_props.clone(),
            self.last_event_id.clone(),
        )
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
    event_parser: EventParser,
    last_event_id: String,
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
    StreamClosed,
}

impl State {
    fn name(&self) -> &'static str {
        match self {
            State::StreamClosed => "closed",
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
    fn new(
        http: hyper::Client<C>,
        props: RequestProps,
        last_event_id: String,
    ) -> ReconnectingRequest<C> {
        let reconnect_delay = props.reconnect_opts.delay;
        ReconnectingRequest {
            props,
            http,
            state: State::New,
            next_reconnect_delay: reconnect_delay,
            event_parser: EventParser::new(),
            last_event_id,
        }
    }
}

impl<C> ReconnectingRequest<C> {
    fn send_request(&self) -> Result<ResponseFuture>
    where
        C: Connect + Clone + Send + Sync + 'static,
    {
        let mut request_builder = Request::builder()
            .method(self.props.method.as_str())
            .uri(&self.props.url);

        for (name, value) in &self.props.headers {
            request_builder = request_builder.header(name, value);
        }
        if !self.last_event_id.is_empty() {
            request_builder = request_builder.header(
                "last-event-id",
                HeaderValue::from_str(&self.last_event_id.clone()).unwrap(),
            );
        }

        let mut body = Body::empty();

        if let Some(props_body) = &self.props.body {
            body = Body::from(props_body.to_string());
        }

        let request = request_builder
            .body(body)
            .map_err(|e| Error::HttpRequest(Box::new(e)))?;

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
    type Item = Result<SSE>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        trace!("ReconnectingRequest::poll({:?})", &self.state);

        loop {
            let this = self.as_mut().project();
            if let Some(event) = this.event_parser.get_event() {
                match event {
                    SSE::Event(ref evt) => {
                        if !evt.id.is_empty() {
                            *this.last_event_id = String::from_utf8(evt.id.clone()).unwrap();
                        }

                        if let Some(retry) = evt.retry {
                            this.props.reconnect_opts.delay = Duration::from_millis(retry);
                            self.as_mut().reset_backoff();
                        }
                        return Poll::Ready(Some(Ok(event)));
                    }
                    SSE::Comment(_) => return Poll::Ready(Some(Ok(event))),
                };
            }

            trace!("ReconnectingRequest::poll loop({:?})", &this.state);

            let state = this.state.project();
            match state {
                StateProj::StreamClosed => return Poll::Ready(Some(Err(Error::StreamClosed))),
                // New immediately transitions to Connecting, and exists only
                // to ensure that we only connect when polled.
                StateProj::New => {
                    *self.as_mut().project().event_parser = EventParser::new();
                    match self.send_request() {
                        Ok(resp) => {
                            let retry = self.props.reconnect_opts.retry_initial;
                            self.as_mut()
                                .project()
                                .state
                                .set(State::Connecting { resp, retry })
                        }
                        Err(e) => {
                            // This error seems to be unrecoverable. So we should just shut down the
                            // stream.
                            self.as_mut().project().state.set(State::StreamClosed);
                            return Poll::Ready(Some(Err(e)));
                        }
                    }
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
                            .set(State::Connected(resp.into_body()));
                    }
                    Err(e) => {
                        // This seems basically impossible. AFAIK we can only get this way if we
                        // poll after it was already ready
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
                        this.event_parser.process_bytes(result)?;
                        continue;
                    }
                    Some(Err(e)) => {
                        if self.props.reconnect_opts.reconnect {
                            let duration = self.as_mut().backoff();
                            self.as_mut()
                                .project()
                                .state
                                .set(State::WaitingToReconnect(delay(duration, "reconnecting")));
                        }

                        if let Some(cause) = e.source() {
                            if let Some(downcast) = cause.downcast_ref::<std::io::Error>() {
                                if let std::io::ErrorKind::TimedOut = downcast.kind() {
                                    return Poll::Ready(Some(Err(Error::TimedOut)));
                                }
                            }
                        } else {
                            return Poll::Ready(Some(Err(Error::HttpStream(Box::new(e)))));
                        }
                    }
                    None => {
                        let duration = self.as_mut().backoff();
                        self.as_mut()
                            .project()
                            .state
                            .set(State::WaitingToReconnect(delay(duration, "retrying")));

                        if self.event_parser.was_processing() {
                            return Poll::Ready(Some(Err(Error::UnexpectedEof)));
                        }
                        return Poll::Ready(Some(Err(Error::Eof)));
                    }
                },
                StateProj::WaitingToReconnect(delay) => {
                    ready!(delay.poll(cx));
                    info!("Reconnecting");
                    self.as_mut().project().state.set(State::New);
                }
            };
        }
    }
}
