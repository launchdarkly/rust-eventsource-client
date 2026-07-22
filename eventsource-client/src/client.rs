use base64::prelude::*;

use futures::{ready, Stream};
use http::{HeaderMap, HeaderName, HeaderValue, Request, Uri};
use log::{debug, info, trace, warn};
use pin_project::pin_project;
use std::{
    boxed,
    fmt::{self, Debug, Formatter},
    future::Future,
    io::ErrorKind,
    pin::Pin,
    str::FromStr,
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use tokio::sync::watch;
use tokio::time::Sleep;

use crate::{
    config::ReconnectOptions,
    response::{ErrorBody, Response},
};
use crate::{
    error::{Error, Result},
    event_parser::ConnectionDetails,
};
use launchdarkly_sdk_transport::{ByteStream, HttpTransport, ResponseFuture};

use crate::event_parser::EventParser;
use crate::event_parser::SSE;

use crate::retry::{BackoffRetry, RetryStrategy};
use std::error::Error as StdError;

/// Represents a [`Pin`]'d [`Send`] + [`Sync`] stream, returned by [`Client`]'s stream method.
pub type BoxStream<T> = Pin<boxed::Box<dyn Stream<Item = T> + Send + Sync>>;

/// Client is the Server-Sent-Events interface.
/// This trait is sealed and cannot be implemented for types outside this crate.
pub trait Client: Send + Sync + private::Sealed {
    fn stream(&self) -> BoxStream<Result<SSE>>;
}

/*
 * TODO remove debug output
 * TODO specify list of stati to not retry (e.g. 204)
 */

/// Maximum amount of redirects that the client will follow before
/// giving up, if not overridden via [ClientBuilder::redirect_limit].
pub const DEFAULT_REDIRECT_LIMIT: u32 = 16;

/// ClientBuilder provides a series of builder methods to easily construct a [`Client`].
pub struct ClientBuilder {
    url: Uri,
    headers: HeaderMap,
    reconnect_opts: ReconnectOptions,
    last_event_id: Option<String>,
    method: String,
    body: Option<String>,
    max_redirects: Option<u32>,
    dynamic_url: Option<watch::Receiver<Uri>>,
}

impl ClientBuilder {
    /// Create a builder for a given URL.
    pub fn for_url(url: &str) -> Result<ClientBuilder> {
        let url = url
            .parse()
            .map_err(|e| Error::InvalidParameter(Box::new(e)))?;

        let mut header_map = HeaderMap::new();
        header_map.insert("Accept", HeaderValue::from_static("text/event-stream"));
        header_map.insert("Cache-Control", HeaderValue::from_static("no-cache"));

        Ok(ClientBuilder {
            url,
            headers: header_map,
            reconnect_opts: ReconnectOptions::default(),
            last_event_id: None,
            method: String::from("GET"),
            max_redirects: None,
            body: None,
            dynamic_url: None,
        })
    }

    /// Watch the given receiver for the url to use when attempting to connect
    /// or reconnect. Overrides [`for_url`] if both are present.
    pub fn dynamic_url(mut self, uri: watch::Receiver<Uri>) -> ClientBuilder {
        self.dynamic_url = Some(uri);
        self
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
        self.last_event_id = Some(last_event_id);
        self
    }

    /// Set a HTTP header on the SSE request.
    pub fn header(mut self, name: &str, value: &str) -> Result<ClientBuilder> {
        let name = HeaderName::from_str(name).map_err(|e| Error::InvalidParameter(Box::new(e)))?;

        let value =
            HeaderValue::from_str(value).map_err(|e| Error::InvalidParameter(Box::new(e)))?;

        self.headers.insert(name, value);
        Ok(self)
    }

    /// Set the Authorization header with the calculated basic authentication value.
    pub fn basic_auth(self, username: &str, password: &str) -> Result<ClientBuilder> {
        let auth = format!("{username}:{password}");
        let encoded = BASE64_STANDARD.encode(auth);
        let value = format!("Basic {encoded}");

        self.header("Authorization", &value)
    }

    /// Configure the client's reconnect behaviour according to the supplied
    /// [`ReconnectOptions`].
    ///
    /// [`ReconnectOptions`]: struct.ReconnectOptions.html
    pub fn reconnect(mut self, opts: ReconnectOptions) -> ClientBuilder {
        self.reconnect_opts = opts;
        self
    }

    /// Customize the client's following behavior when served a redirect.
    /// To disable following redirects, pass `0`.
    /// By default, the limit is [`DEFAULT_REDIRECT_LIMIT`].
    pub fn redirect_limit(mut self, limit: u32) -> ClientBuilder {
        self.max_redirects = Some(limit);
        self
    }

    /// Build a client with a custom HTTP transport implementation.
    ///
    /// # Arguments
    ///
    /// * `transport` - An implementation of the [`HttpTransport`] trait that will handle
    ///   HTTP requests. See the `examples/` directory for reference implementations.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use eventsource_client::ClientBuilder;
    ///
    /// let transport = MyTransport::new();
    /// let client = ClientBuilder::for_url("https://live-test-scores.herokuapp.com/scores")
    ///     .expect("failed to create client builder")
    ///     .build_with_transport(transport);
    /// ```
    pub fn build_with_transport<T>(self, transport: T) -> impl Client
    where
        T: HttpTransport,
    {
        ClientImpl {
            transport: Arc::new(transport),
            request_props: RequestProps {
                url: self.url,
                headers: self.headers,
                method: self.method,
                body: self.body,
                reconnect_opts: self.reconnect_opts,
                max_redirects: self.max_redirects.unwrap_or(DEFAULT_REDIRECT_LIMIT),
                dynamic_url: self.dynamic_url,
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
    max_redirects: u32,
    dynamic_url: Option<watch::Receiver<Uri>>,
}

impl RequestProps {
    fn resolve_url(&self) -> Uri {
        self.dynamic_url
            .as_ref()
            .map(|rx| rx.borrow().clone())
            .unwrap_or_else(|| self.url.clone())
    }
}

/// A client implementation that connects to a server using the Server-Sent Events protocol
/// and consumes the event stream indefinitely.
struct ClientImpl<T: HttpTransport> {
    transport: Arc<T>,
    request_props: RequestProps,
    last_event_id: Option<String>,
}

impl<T: HttpTransport> Client for ClientImpl<T> {
    /// Connect to the server and begin consuming the stream. Produces a
    /// [`Stream`] of [`Event`](crate::Event)s wrapped in [`Result`].
    ///
    /// Errors yielded by the stream are not terminal: keep polling.
    /// When [`ReconnectOptions::reconnect`] is enabled (the default),
    /// the stream schedules a reconnect on retryable errors and the
    /// next poll resumes from a fresh connection.
    ///
    /// The stream is exhausted only when [`Stream::poll_next`] returns
    /// [`Poll::Ready(None)`]. That happens when the underlying state
    /// machine reaches `StreamClosed` (e.g. a redirect-limit overrun,
    /// a malformed `Location` header, or an error during initial
    /// connection while [`ReconnectOptions::retry_initial`] is
    /// disabled), or after any error when reconnect is disabled.
    ///
    /// [`Poll::Ready(None)`]: std::task::Poll::Ready
    /// [`Stream::poll_next`]: futures::Stream::poll_next
    fn stream(&self) -> BoxStream<Result<SSE>> {
        Box::pin(ReconnectingRequest::new(
            Arc::clone(&self.transport),
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
        redirect_count: u32,
        #[pin]
        resp: ResponseFuture,
    },
    Connected(#[pin] ByteStream),
    WaitingToReconnect(#[pin] Sleep),
    FollowingRedirect {
        header: Option<HeaderValue>,
        redirect_count: u32,
    },
    StreamClosed,
}

impl State {
    fn name(&self) -> &'static str {
        match self {
            State::New => "new",
            State::Connecting { retry: false, .. } => "connecting(no-retry)",
            State::Connecting { retry: true, .. } => "connecting(retry)",
            State::Connected(_) => "connected",
            State::WaitingToReconnect(_) => "waiting-to-reconnect",
            State::FollowingRedirect { .. } => "following-redirect",
            State::StreamClosed => "closed",
        }
    }
}

impl Debug for State {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

#[must_use = "streams do nothing unless polled"]
#[pin_project]
pub struct ReconnectingRequest<T: HttpTransport> {
    transport: Arc<T>,
    props: RequestProps,
    #[pin]
    state: State,
    retry_strategy: Box<dyn RetryStrategy + Send + Sync>,
    event_parser: EventParser,
    last_event_id: Option<String>,
    #[pin]
    initial_connection: bool,
}

impl<T: HttpTransport> ReconnectingRequest<T> {
    fn new(
        transport: Arc<T>,
        props: RequestProps,
        last_event_id: Option<String>,
    ) -> ReconnectingRequest<T> {
        let reconnect_delay = props.reconnect_opts.delay;
        let delay_max = props.reconnect_opts.delay_max;
        let backoff_factor = props.reconnect_opts.backoff_factor;

        ReconnectingRequest {
            props,
            transport,
            state: State::New,
            retry_strategy: Box::new(BackoffRetry::new(
                reconnect_delay,
                delay_max,
                backoff_factor,
                true,
            )),
            event_parser: EventParser::new(),
            last_event_id,
            initial_connection: true,
        }
    }

    fn send_request(&self, url: &Uri) -> Result<ResponseFuture> {
        let mut request_builder = Request::builder()
            .method(self.props.method.as_str())
            .uri(url);

        for (name, value) in &self.props.headers {
            request_builder = request_builder.header(name, value);
        }

        if let Some(id) = self.last_event_id.as_ref() {
            if !id.is_empty() {
                let id_as_header =
                    HeaderValue::from_str(id).map_err(|e| Error::InvalidParameter(Box::new(e)))?;

                request_builder = request_builder.header("last-event-id", id_as_header);
            }
        }

        // Include the request body if set. Most SSE requests use GET and will have None,
        // but some implementations (e.g., using REPORT method) may include a body.
        let request = request_builder
            .body(self.props.body.clone().map(|b| b.into()))
            .map_err(|e| Error::InvalidParameter(Box::new(e)))?;

        Ok(self.transport.request(request))
    }
}

impl<T: HttpTransport> Stream for ReconnectingRequest<T> {
    type Item = Result<SSE>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        trace!("ReconnectingRequest::poll({:?})", &self.state);

        loop {
            let this = self.as_mut().project();
            if let Some(event) = this.event_parser.get_event() {
                return match event {
                    SSE::Connected(_) => Poll::Ready(Some(Ok(event))),
                    SSE::Event(ref evt) => {
                        this.last_event_id.clone_from(&evt.id);

                        if let Some(retry) = evt.retry {
                            this.retry_strategy
                                .change_base_delay(Duration::from_millis(retry));
                        }
                        Poll::Ready(Some(Ok(event)))
                    }
                    SSE::Comment(_) => Poll::Ready(Some(Ok(event))),
                };
            }

            trace!("ReconnectingRequest::poll loop({:?})", &this.state);

            let state = this.state.project();
            match state {
                StateProj::StreamClosed => return Poll::Ready(None),
                // New immediately transitions to Connecting, and exists only
                // to ensure that we only connect when polled.
                StateProj::New => {
                    *self.as_mut().project().event_parser = EventParser::new();
                    let url = self.props.resolve_url();
                    match self.send_request(&url) {
                        Ok(resp) => {
                            let retry = if self.initial_connection {
                                self.props.reconnect_opts.retry_initial
                            } else {
                                self.props.reconnect_opts.reconnect
                            };
                            self.as_mut().project().state.set(State::Connecting {
                                resp,
                                retry,
                                redirect_count: 0,
                            })
                        }
                        Err(e) => {
                            // This error seems to be unrecoverable. So we should just shut down the
                            // stream.
                            self.as_mut().project().state.set(State::StreamClosed);
                            return Poll::Ready(Some(Err(e)));
                        }
                    }
                }
                StateProj::Connecting {
                    retry,
                    redirect_count,
                    resp,
                } => match ready!(resp.poll(cx)) {
                    Ok(resp) => {
                        debug!(
                            "HTTP response status: {}, headers: {:?}",
                            resp.status(),
                            resp.headers()
                        );

                        if resp.status().is_success() {
                            self.as_mut().project().retry_strategy.reset(Instant::now());

                            let status = resp.status();
                            let headers = resp.headers().clone();

                            self.as_mut()
                                .project()
                                .state
                                .set(State::Connected(resp.into_body()));
                            self.as_mut().project().initial_connection.set(false);

                            return Poll::Ready(Some(Ok(SSE::Connected(ConnectionDetails::new(
                                Response::new(status, headers),
                            )))));
                        }

                        if resp.status() == 301 || resp.status() == 307 {
                            debug!("got redirected ({})", resp.status());

                            let next_count = *redirect_count + 1;
                            if next_count > self.props.max_redirects {
                                debug!("redirect limit reached ({})", self.props.max_redirects);

                                self.as_mut().project().state.set(State::StreamClosed);
                                return Poll::Ready(Some(Err(Error::MaxRedirectLimitReached(
                                    self.props.max_redirects,
                                ))));
                            } else {
                                debug!("following redirect {}", next_count);

                                self.as_mut().project().state.set(State::FollowingRedirect {
                                    header: resp.headers().get("location").cloned(),
                                    redirect_count: next_count,
                                });
                                continue;
                            }
                        }

                        let status = resp.status();
                        let headers = resp.headers().clone();
                        let body = resp.into_body();

                        let error = Error::UnexpectedResponse(
                            Response::new(status, headers),
                            ErrorBody::new(body),
                        );

                        if !*retry {
                            self.as_mut().project().state.set(State::StreamClosed);
                            return Poll::Ready(Some(Err(error)));
                        }

                        let duration = self
                            .as_mut()
                            .project()
                            .retry_strategy
                            .next_delay(Instant::now());

                        self.as_mut()
                            .project()
                            .state
                            .set(State::WaitingToReconnect(delay(duration, "retrying")));

                        return Poll::Ready(Some(Err(error)));
                    }
                    Err(e) => {
                        // This happens when the server is unreachable, e.g. connection refused.
                        warn!("request returned an error: {e}");
                        if !*retry {
                            self.as_mut().project().state.set(State::StreamClosed);
                            return Poll::Ready(Some(Err(Error::Transport(e))));
                        }

                        let duration = self
                            .as_mut()
                            .project()
                            .retry_strategy
                            .next_delay(Instant::now());

                        self.as_mut()
                            .project()
                            .state
                            .set(State::WaitingToReconnect(delay(duration, "retrying")));
                    }
                },
                StateProj::FollowingRedirect {
                    header,
                    redirect_count,
                } => match uri_from_header(header) {
                    Ok(uri) => {
                        let count = *redirect_count;
                        match self.send_request(&uri) {
                            Ok(resp) => {
                                let retry = if self.initial_connection {
                                    self.props.reconnect_opts.retry_initial
                                } else {
                                    self.props.reconnect_opts.reconnect
                                };
                                self.as_mut().project().state.set(State::Connecting {
                                    resp,
                                    retry,
                                    redirect_count: count,
                                });
                            }
                            Err(e) => {
                                self.as_mut().project().state.set(State::StreamClosed);
                                return Poll::Ready(Some(Err(e)));
                            }
                        }
                    }
                    Err(e) => {
                        self.as_mut().project().state.set(State::StreamClosed);
                        return Poll::Ready(Some(Err(e)));
                    }
                },
                StateProj::Connected(mut body) => match ready!(body.as_mut().poll_next(cx)) {
                    Some(Ok(result)) => {
                        if let Err(e) = this.event_parser.process_bytes(result) {
                            // The current response body is unusable. Either
                            // schedule a reconnect or close the stream so a
                            // caller that disabled reconnect doesn't keep
                            // reading from a poisoned parser.
                            if self.props.reconnect_opts.reconnect {
                                let duration = self
                                    .as_mut()
                                    .project()
                                    .retry_strategy
                                    .next_delay(Instant::now());
                                self.as_mut().project().state.set(State::WaitingToReconnect(
                                    delay(duration, "reconnecting"),
                                ));
                            } else {
                                self.as_mut().project().state.set(State::StreamClosed);
                            }
                            return Poll::Ready(Some(Err(e)));
                        }
                        continue;
                    }
                    Some(Err(e)) => {
                        if self.props.reconnect_opts.reconnect {
                            let duration = self
                                .as_mut()
                                .project()
                                .retry_strategy
                                .next_delay(Instant::now());
                            self.as_mut()
                                .project()
                                .state
                                .set(State::WaitingToReconnect(delay(duration, "reconnecting")));
                        }

                        // Check if the underlying error is a timeout
                        if let Some(cause) = e.source() {
                            if let Some(downcast) = cause.downcast_ref::<std::io::Error>() {
                                if let std::io::ErrorKind::TimedOut = downcast.kind() {
                                    return Poll::Ready(Some(Err(Error::TimedOut)));
                                }
                            }
                        }

                        return Poll::Ready(Some(Err(Error::Transport(e))));
                    }
                    None => {
                        if self.props.reconnect_opts.reconnect {
                            let duration = self
                                .as_mut()
                                .project()
                                .retry_strategy
                                .next_delay(Instant::now());
                            self.as_mut()
                                .project()
                                .state
                                .set(State::WaitingToReconnect(delay(duration, "retrying")));
                        } else {
                            self.as_mut().project().state.set(State::StreamClosed);
                        }

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

fn uri_from_header(maybe_header: &Option<HeaderValue>) -> Result<Uri> {
    let header = maybe_header.as_ref().ok_or_else(|| {
        Error::MalformedLocationHeader(Box::new(std::io::Error::new(
            ErrorKind::NotFound,
            "missing Location header",
        )))
    })?;

    let header_string = header
        .to_str()
        .map_err(|e| Error::MalformedLocationHeader(Box::new(e)))?;

    header_string
        .parse::<Uri>()
        .map_err(|e| Error::MalformedLocationHeader(Box::new(e)))
}

fn delay(dur: Duration, description: &str) -> Sleep {
    info!("Waiting {dur:?} before {description}");
    tokio::time::sleep(dur)
}

mod private {
    use crate::client::ClientImpl;
    use launchdarkly_sdk_transport::HttpTransport;

    pub trait Sealed {}
    impl<T: HttpTransport> Sealed for ClientImpl<T> {}
}

#[cfg(test)]
mod tests {
    use crate::ClientBuilder;
    use http::HeaderValue;
    use test_case::test_case;

    #[test_case("user", "pass", "dXNlcjpwYXNz")]
    #[test_case("user1", "password123", "dXNlcjE6cGFzc3dvcmQxMjM=")]
    #[test_case("user2", "", "dXNlcjI6")]
    #[test_case("user@name", "pass#word!", "dXNlckBuYW1lOnBhc3Mjd29yZCE=")]
    #[test_case("user3", "my pass", "dXNlcjM6bXkgcGFzcw==")]
    #[test_case(
        "weird@-/:stuff",
        "goes@-/:here",
        "d2VpcmRALS86c3R1ZmY6Z29lc0AtLzpoZXJl"
    )]
    fn basic_auth_generates_correct_headers(username: &str, password: &str, expected: &str) {
        let builder = ClientBuilder::for_url("http://example.com")
            .expect("failed to build client")
            .basic_auth(username, password)
            .expect("failed to add authentication");

        let actual = builder.headers.get("Authorization");
        let expected = HeaderValue::from_str(format!("Basic {expected}").as_str())
            .expect("unable to create expected header");

        assert_eq!(Some(&expected), actual);
    }

    use std::{
        pin::pin,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use bytes::Bytes;
    use futures::{stream, TryStreamExt};
    use http::HeaderMap;
    use tokio::time::timeout;

    use crate::{
        client::{RequestProps, State},
        ReconnectOptionsBuilder, ReconnectingRequest,
    };
    use launchdarkly_sdk_transport::{ByteStream, HttpTransport, ResponseFuture, TransportError};

    // Mock transport for testing
    #[derive(Clone)]
    struct MockTransport {
        fail_request: bool,
    }

    impl MockTransport {
        fn new(_url: String, fail_request: bool) -> Self {
            Self { fail_request }
        }
    }

    impl HttpTransport for MockTransport {
        fn request(&self, _request: http::Request<Option<Bytes>>) -> ResponseFuture {
            if self.fail_request {
                // Simulate a connection error
                Box::pin(async {
                    Err(TransportError::new(std::io::Error::new(
                        std::io::ErrorKind::ConnectionRefused,
                        "connection refused",
                    )))
                })
            } else {
                // Return a 404 response
                Box::pin(async {
                    let byte_stream: ByteStream =
                        Box::pin(stream::iter(vec![Ok(Bytes::from("not found"))]));
                    let response = http::Response::builder()
                        .status(404)
                        .body(byte_stream)
                        .unwrap();
                    Ok(response)
                })
            }
        }
    }

    const INVALID_URI: &str = "http://mycrazyunexsistenturl.invaliddomainext";

    #[test_case(INVALID_URI, false, |state| matches!(state, State::StreamClosed))]
    #[test_case(INVALID_URI, true, |state| matches!(state, State::WaitingToReconnect(_)))]
    #[tokio::test]
    async fn initial_connection(uri: &str, retry_initial: bool, expected: fn(&State) -> bool) {
        let reconnect_opts = ReconnectOptionsBuilder::new(false)
            .backoff_factor(1)
            .delay(Duration::from_secs(1))
            .retry_initial(retry_initial)
            .build();

        let transport = Arc::new(MockTransport::new(uri.to_string(), true));
        let req_props = RequestProps {
            url: uri.parse().unwrap(),
            headers: HeaderMap::new(),
            method: "GET".to_string(),
            body: None,
            reconnect_opts,
            max_redirects: 10,
            dynamic_url: None,
        };

        let mut reconnecting_request = ReconnectingRequest::new(transport.clone(), req_props, None);

        // sets initial state with a failing request
        let resp = transport.request(http::Request::builder().uri(uri).body(None).unwrap());

        reconnecting_request.state = State::Connecting {
            retry: reconnecting_request.props.reconnect_opts.retry_initial,
            redirect_count: 0,
            resp,
        };

        let mut reconnecting_request = pin!(reconnecting_request);

        timeout(Duration::from_millis(500), reconnecting_request.try_next())
            .await
            .ok();

        assert!(expected(&reconnecting_request.state));
    }

    #[test_case(false, |state| matches!(state, State::StreamClosed))]
    #[test_case(true, |state| matches!(state, State::WaitingToReconnect(_)))]
    #[tokio::test]
    async fn initial_connection_mocked_server(retry_initial: bool, expected: fn(&State) -> bool) {
        let mut mock_server = mockito::Server::new_async().await;
        let _mock = mock_server
            .mock("GET", "/")
            .with_status(404)
            .create_async()
            .await;

        initial_connection(&mock_server.url(), retry_initial, expected).await;
    }

    #[derive(Clone)]
    struct CapturingTransport {
        captured_uris: Arc<Mutex<Vec<http::Uri>>>,
    }

    impl HttpTransport for CapturingTransport {
        fn request(&self, request: http::Request<Option<Bytes>>) -> ResponseFuture {
            self.captured_uris
                .lock()
                .unwrap()
                .push(request.uri().clone());
            Box::pin(async {
                Err(TransportError::new(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    "test",
                )))
            })
        }
    }

    fn props_with_dynamic(
        static_url: &str,
        rx: tokio::sync::watch::Receiver<http::Uri>,
    ) -> RequestProps {
        RequestProps {
            url: static_url.parse().unwrap(),
            headers: HeaderMap::new(),
            method: "GET".to_string(),
            body: None,
            reconnect_opts: ReconnectOptionsBuilder::new(false).build(),
            max_redirects: 10,
            dynamic_url: Some(rx),
        }
    }

    #[tokio::test]
    async fn dynamic_url_is_used_on_initial_connect() {
        let (_tx, rx) = tokio::sync::watch::channel(
            "http://dynamic.example.com/".parse::<http::Uri>().unwrap(),
        );
        let captured = Arc::new(Mutex::new(Vec::new()));
        let transport = CapturingTransport {
            captured_uris: captured.clone(),
        };
        let props = props_with_dynamic("http://static.example.com/", rx);
        let req = ReconnectingRequest::new(Arc::new(transport), props, None);

        let _ = req.send_request(&req.props.resolve_url());

        let uris = captured.lock().unwrap();
        assert_eq!(uris.len(), 1);
        assert_eq!(uris[0].to_string(), "http://dynamic.example.com/");
    }

    #[derive(Clone)]
    struct RedirectTransport {
        location: String,
    }

    impl HttpTransport for RedirectTransport {
        fn request(&self, _request: http::Request<Option<Bytes>>) -> ResponseFuture {
            let location = self.location.clone();
            Box::pin(async move {
                let byte_stream: ByteStream = Box::pin(stream::iter(Vec::<
                    std::result::Result<Bytes, TransportError>,
                >::new()));
                Ok(http::Response::builder()
                    .status(301)
                    .header("Location", location)
                    .body(byte_stream)
                    .unwrap())
            })
        }
    }

    #[derive(Clone)]
    struct RedirectOnceTransport {
        location: String,
        captured_uris: Arc<Mutex<Vec<http::Uri>>>,
    }

    impl HttpTransport for RedirectOnceTransport {
        fn request(&self, request: http::Request<Option<Bytes>>) -> ResponseFuture {
            let mut uris = self.captured_uris.lock().unwrap();
            let is_first = uris.is_empty();
            uris.push(request.uri().clone());
            drop(uris);
            let location = self.location.clone();
            Box::pin(async move {
                if is_first {
                    let byte_stream: ByteStream = Box::pin(stream::iter(Vec::<
                        std::result::Result<Bytes, TransportError>,
                    >::new(
                    )));
                    Ok(http::Response::builder()
                        .status(301)
                        .header("Location", location)
                        .body(byte_stream)
                        .unwrap())
                } else {
                    Err(TransportError::new(std::io::Error::new(
                        std::io::ErrorKind::ConnectionRefused,
                        "stop",
                    )))
                }
            })
        }
    }

    #[tokio::test]
    async fn connecting_sees_301_follows_to_location() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let transport = Arc::new(RedirectOnceTransport {
            location: "http://redirect.example.com/".to_string(),
            captured_uris: captured.clone(),
        });
        let props = RequestProps {
            url: "http://start.example.com/".parse().unwrap(),
            headers: HeaderMap::new(),
            method: "GET".to_string(),
            body: None,
            reconnect_opts: ReconnectOptionsBuilder::new(false).build(),
            max_redirects: 3,
            dynamic_url: None,
        };
        let req = ReconnectingRequest::new(transport, props, None);
        let mut req = pin!(req);
        timeout(Duration::from_millis(500), req.try_next())
            .await
            .ok();

        let uris = captured.lock().unwrap();
        assert_eq!(uris.len(), 2);
        assert_eq!(uris[0].to_string(), "http://start.example.com/");
        assert_eq!(uris[1].to_string(), "http://redirect.example.com/");
    }

    #[tokio::test]
    async fn connecting_sees_301_at_redirect_limit_closes_stream() {
        let transport = Arc::new(RedirectTransport {
            location: "http://redirect.example.com/".to_string(),
        });
        let props = RequestProps {
            url: "http://start.example.com/".parse().unwrap(),
            headers: HeaderMap::new(),
            method: "GET".to_string(),
            body: None,
            reconnect_opts: ReconnectOptionsBuilder::new(false).build(),
            max_redirects: 3,
            dynamic_url: None,
        };
        let mut req = ReconnectingRequest::new(transport.clone(), props, None);

        let resp = transport.request(
            http::Request::builder()
                .uri("http://start.example.com/")
                .body(None)
                .unwrap(),
        );
        // Already at max_redirects=3, so the next redirect (would be #4) should fail.
        req.state = State::Connecting {
            retry: true,
            redirect_count: 3,
            resp,
        };

        let mut req = pin!(req);
        let result = timeout(Duration::from_millis(500), req.try_next()).await;

        assert!(matches!(&req.state, State::StreamClosed));
        assert!(matches!(
            result,
            Ok(Err(crate::Error::MaxRedirectLimitReached(3)))
        ));
    }

    #[tokio::test]
    async fn redirect_target_overrides_dynamic_url() {
        let (_tx, rx) = tokio::sync::watch::channel(
            "http://dynamic.example.com/".parse::<http::Uri>().unwrap(),
        );
        let captured = Arc::new(Mutex::new(Vec::new()));
        let transport = CapturingTransport {
            captured_uris: captured.clone(),
        };
        let props = props_with_dynamic("http://static.example.com/", rx);
        let mut req = ReconnectingRequest::new(Arc::new(transport), props, None);

        // Jump straight to FollowingRedirect. The poll loop should parse the
        // location header and call send_request with the redirect target,
        // not with the dynamic-uri watch value.
        req.state = State::FollowingRedirect {
            header: Some(http::HeaderValue::from_static(
                "http://redirect.example.com/",
            )),
            redirect_count: 1,
        };

        let mut req = pin!(req);
        timeout(Duration::from_millis(500), req.try_next())
            .await
            .ok();

        let uris = captured.lock().unwrap();
        assert_eq!(uris.len(), 1);
        assert_eq!(uris[0].to_string(), "http://redirect.example.com/");
    }

    #[tokio::test]
    async fn updated_dynamic_url_is_used_on_next_send_request() {
        let (tx, rx) =
            tokio::sync::watch::channel("http://v1.example.com/".parse::<http::Uri>().unwrap());
        let captured = Arc::new(Mutex::new(Vec::new()));
        let transport = CapturingTransport {
            captured_uris: captured.clone(),
        };
        let props = props_with_dynamic("http://static.example.com/", rx);
        let req = ReconnectingRequest::new(Arc::new(transport), props, None);

        let _ = req.send_request(&req.props.resolve_url());
        tx.send("http://v2.example.com/".parse().unwrap()).unwrap();
        let _ = req.send_request(&req.props.resolve_url());

        let uris = captured.lock().unwrap();
        assert_eq!(uris.len(), 2);
        assert_eq!(uris[0].to_string(), "http://v1.example.com/");
        assert_eq!(uris[1].to_string(), "http://v2.example.com/");
    }

    // When a parse error happens during streaming and reconnect is
    // enabled, the next stream item should be a fresh `Connected` from
    // the reconnect, not another error from continuing to drain the
    // broken response body.
    #[cfg(feature = "hyper")]
    #[tokio::test(flavor = "multi_thread")]
    async fn parser_error_schedules_reconnect_immediately() {
        use crate::{Client, ClientBuilder, Error, ReconnectOptionsBuilder, SSE};
        use futures::StreamExt;
        use launchdarkly_sdk_transport::HyperTransport;

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/")
            .with_status(200)
            .with_body(b"\xff\xfe:bad\n\n".as_ref())
            .create_async()
            .await;

        let transport = HyperTransport::new().expect("failed to build transport");
        let client = ClientBuilder::for_url(&server.url())
            .unwrap()
            .reconnect(
                ReconnectOptionsBuilder::new(true)
                    .delay(Duration::from_millis(10))
                    .delay_max(Duration::from_millis(10))
                    .retry_initial(true)
                    .build(),
            )
            .build_with_transport(transport);

        let mut stream = client.stream();

        // Expected order: Connected, parse error, Connected (reconnect).
        let mut items = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), async {
            while items.len() < 3 {
                match stream.next().await {
                    Some(item) => items.push(item),
                    None => break,
                }
            }
        })
        .await
        .expect("timed out waiting for parse error and reconnect");

        assert!(
            matches!(items.first(), Some(Ok(SSE::Connected(_)))),
            "expected initial Connected, got {:?}",
            items.first()
        );
        assert!(
            matches!(items.get(1), Some(Err(Error::InvalidLine(_)))),
            "expected InvalidLine error after first connection, got {:?}",
            items.get(1)
        );
        assert!(
            matches!(items.get(2), Some(Ok(SSE::Connected(_)))),
            "expected reconnect (Connected) immediately after parse error, got {:?}",
            items.get(2)
        );
    }

    // With reconnect disabled, a parse error should close the stream so the
    // next poll returns `None` rather than continuing to read from a poisoned
    // parser or reconnecting via the EOF arm.
    #[cfg(feature = "hyper")]
    #[tokio::test(flavor = "multi_thread")]
    async fn parser_error_closes_stream_when_reconnect_disabled() {
        use crate::{Client, ClientBuilder, Error, ReconnectOptionsBuilder, SSE};
        use futures::StreamExt;
        use launchdarkly_sdk_transport::HyperTransport;

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/")
            .with_status(200)
            .with_body(b"\xff\xfe:bad\n\n".as_ref())
            .create_async()
            .await;

        let transport = HyperTransport::new().expect("failed to build transport");
        let client = ClientBuilder::for_url(&server.url())
            .unwrap()
            .reconnect(
                ReconnectOptionsBuilder::new(false)
                    .retry_initial(true)
                    .build(),
            )
            .build_with_transport(transport);

        let mut stream = client.stream();

        let mut items = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), async {
            while items.len() < 3 {
                match stream.next().await {
                    Some(item) => items.push(item),
                    None => {
                        items.push(Ok(SSE::Comment("__stream_ended__".into())));
                        break;
                    }
                }
            }
        })
        .await
        .expect("timed out waiting for stream to close");

        assert!(
            matches!(items.first(), Some(Ok(SSE::Connected(_)))),
            "expected initial Connected, got {:?}",
            items.first()
        );
        assert!(
            matches!(items.get(1), Some(Err(Error::InvalidLine(_)))),
            "expected InvalidLine error, got {:?}",
            items.get(1)
        );
        assert!(
            matches!(
                items.get(2),
                Some(Ok(SSE::Comment(s))) if s == "__stream_ended__"
            ),
            "expected stream to end (None) after parse error with reconnect disabled, got {:?}",
            items.get(2)
        );
    }

    // With reconnect disabled, a clean end-of-body should close the stream
    // rather than scheduling a reconnect.
    #[cfg(feature = "hyper")]
    #[tokio::test(flavor = "multi_thread")]
    async fn eof_closes_stream_when_reconnect_disabled() {
        use crate::{Client, ClientBuilder, Error, ReconnectOptionsBuilder, SSE};
        use futures::StreamExt;
        use launchdarkly_sdk_transport::HyperTransport;

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/")
            .with_status(200)
            .with_body("event: hello\ndata: world\n\n")
            .create_async()
            .await;

        let transport = HyperTransport::new().expect("failed to build transport");
        let client = ClientBuilder::for_url(&server.url())
            .unwrap()
            .reconnect(
                ReconnectOptionsBuilder::new(false)
                    .retry_initial(true)
                    .build(),
            )
            .build_with_transport(transport);

        let mut stream = client.stream();

        let mut items: Vec<Option<crate::Result<SSE>>> = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), async {
            for _ in 0..4 {
                let item = stream.next().await;
                let is_terminal = item.is_none();
                items.push(item);
                if is_terminal {
                    break;
                }
            }
        })
        .await
        .expect("timed out waiting for stream to close");

        assert!(
            matches!(items.first(), Some(Some(Ok(SSE::Connected(_))))),
            "expected initial Connected, got {:?}",
            items.first()
        );
        assert!(
            matches!(items.get(1), Some(Some(Ok(SSE::Event(e)))) if e.event_type == "hello"),
            "expected hello event, got {:?}",
            items.get(1)
        );
        assert!(
            matches!(items.get(2), Some(Some(Err(Error::Eof)))),
            "expected Eof error after body ends, got {:?}",
            items.get(2)
        );
        assert!(
            matches!(items.get(3), Some(None)),
            "expected stream to end (None) after EOF with reconnect disabled, got {:?}",
            items.get(3)
        );
    }
}
