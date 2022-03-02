use futures::{ready, Stream};
use hyper::{
    body::HttpBody,
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
use log::{debug, info, trace, warn};
use pin_project::pin_project;
use std::{
    boxed,
    fmt::{self, Debug, Display, Formatter},
    future::Future,
    mem,
    pin::Pin,
    str::FromStr,
    task::{Context, Poll},
    time::Duration,
};

use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::Sleep,
};

use crate::config::ReconnectOptions;
use crate::error::{Error, Result};

use crate::event_parser::EventParser;
use crate::event_parser::SSE;
pub use hyper::client::HttpConnector;
use hyper_timeout::TimeoutConnector;

use std::error::Error as StdError;

#[cfg(feature = "rustls")]
pub type HttpsConnector = RustlsConnector<HttpConnector>;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Represents a [`Pin`]'d [`Send`] + [`Sync`] stream, returned by [`Client`]'s stream method.
pub type BoxStream<T> = Pin<boxed::Box<dyn Stream<Item = T> + Send + Sync>>;

/// Client is the Server-Sent-Events interface.
/// This trait is sealed and cannot be implemented for types outside this crate.
pub trait Client: Send + Sync + private::Sealed {
    /// Returns a stream of [`Event`]s.
    fn stream(&self) -> BoxStream<Result<SSE>>;
}

/*
 * TODO remove debug output
 * TODO specify list of stati to not retry (e.g. 204)
 */

/// ClientBuilder provides a series of builder methods to easily construct a [`Client`].
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
    /// Create a builder for a given URL.
    pub fn for_url(url: &str) -> Result<ClientBuilder> {
        let url = url
            .parse()
            .map_err(|_| Error::HttpRequest(StatusCode::BAD_REQUEST))?;

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

    /// Build with a specific client connector.
    pub fn build_with_conn<C>(self, conn: C) -> impl Client
    where
        C: Service<Uri> + Clone + Send + Sync + 'static,
        C::Response: Connection + AsyncRead + AsyncWrite + Send + Unpin,
        C::Future: Send + 'static,
        C::Error: Into<BoxError>,
    {
        let mut connector = TimeoutConnector::new(conn);
        connector.set_read_timeout(self.read_timeout);

        let client = hyper::Client::builder().build::<_, hyper::Body>(connector);

        ClientImpl {
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

    /// Build with an HTTP client connector.
    pub fn build_http(self) -> impl Client {
        self.build_with_conn(HttpConnector::new())
    }

    #[cfg(feature = "rustls")]
    /// Build with an HTTPS client connector, using the OS root certificate store.
    pub fn build(self) -> impl Client {
        let conn = HttpsConnector::with_native_roots();
        self.build_with_conn(conn)
    }

    /// Build with the given [`hyper::client::Client`].
    pub fn build_with_http_client<C>(self, http: hyper::Client<C>) -> impl Client
    where
        C: Connect + Clone + Send + Sync + 'static,
    {
        ClientImpl {
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

/// A client implementation that connects to a server using the Server-Sent Events protocol
/// and consumes the event stream indefinitely.
/// Can be parameterized with different hyper Connectors, such as HTTP or HTTPS.
struct ClientImpl<C> {
    http: hyper::Client<C>,
    request_props: RequestProps,
    last_event_id: String,
}

impl<C> Client for ClientImpl<C>
where
    C: Connect + Clone + Send + Sync + 'static,
{
    /// Connect to the server and begin consuming the stream. Produces a
    /// [`Stream`] of [`Event`](crate::Event)s wrapped in [`Result`].
    ///
    /// Do not use the stream after it returned an error!
    ///
    /// After the first successful connection, the stream will
    /// reconnect for retryable errors.

    fn stream(&self) -> BoxStream<Result<SSE>> {
        Box::pin(ReconnectingRequest::new(
            self.http.clone(),
            self.request_props.clone(),
            self.last_event_id.clone(),
        ))
    }
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

        // todo(cwaldren): Should this be BAD_REQUEST?
        let request = request_builder
            .body(body)
            .map_err(|_| Error::HttpRequest(StatusCode::BAD_REQUEST))?;

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

mod private {
    use crate::client::ClientImpl;

    pub trait Sealed {}
    impl<C> Sealed for ClientImpl<C> {}
}
