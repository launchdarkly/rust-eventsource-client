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
        })
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
    /// Do not use the stream after it returned an error!
    ///
    /// After the first successful connection, the stream will
    /// reconnect for retryable errors.
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
        #[pin]
        resp: ResponseFuture,
    },
    Connected(#[pin] ByteStream),
    WaitingToReconnect(#[pin] Sleep),
    FollowingRedirect(Option<HeaderValue>),
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
            State::FollowingRedirect(_) => "following-redirect",
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
    current_url: Uri,
    redirect_count: u32,
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

        let url = props.url.clone();
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
            redirect_count: 0,
            current_url: url,
            event_parser: EventParser::new(),
            last_event_id,
            initial_connection: true,
        }
    }

    fn send_request(&self) -> Result<ResponseFuture> {
        let mut request_builder = Request::builder()
            .method(self.props.method.as_str())
            .uri(&self.current_url);

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

    fn reset_redirects(self: Pin<&mut Self>) {
        let url = self.props.url.clone();
        let this = self.project();
        *this.current_url = url;
        *this.redirect_count = 0;
    }

    fn increment_redirect_counter(self: Pin<&mut Self>) -> bool {
        if self.redirect_count == self.props.max_redirects {
            return false;
        }
        *self.project().redirect_count += 1;
        true
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
                    match self.send_request() {
                        Ok(resp) => {
                            let retry = if self.initial_connection {
                                self.props.reconnect_opts.retry_initial
                            } else {
                                self.props.reconnect_opts.reconnect
                            };
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
                        debug!(
                            "HTTP response status: {}, headers: {:?}",
                            resp.status(),
                            resp.headers()
                        );

                        if resp.status().is_success() {
                            self.as_mut().project().retry_strategy.reset(Instant::now());
                            self.as_mut().reset_redirects();

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

                            if self.as_mut().increment_redirect_counter() {
                                debug!("following redirect {}", self.redirect_count);

                                self.as_mut().project().state.set(State::FollowingRedirect(
                                    resp.headers().get("location").cloned(),
                                ));
                                continue;
                            } else {
                                debug!("redirect limit reached ({})", self.props.max_redirects);

                                self.as_mut().project().state.set(State::StreamClosed);
                                return Poll::Ready(Some(Err(Error::MaxRedirectLimitReached(
                                    self.props.max_redirects,
                                ))));
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

                        self.as_mut().reset_redirects();

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
                StateProj::FollowingRedirect(maybe_header) => match uri_from_header(maybe_header) {
                    Ok(uri) => {
                        *self.as_mut().project().current_url = uri;
                        self.as_mut().project().state.set(State::New);
                    }
                    Err(e) => {
                        self.as_mut().project().state.set(State::StreamClosed);
                        return Poll::Ready(Some(Err(e)));
                    }
                },
                StateProj::Connected(mut body) => match ready!(body.as_mut().poll_next(cx)) {
                    Some(Ok(result)) => {
                        this.event_parser.process_bytes(result)?;
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
                        let duration = self
                            .as_mut()
                            .project()
                            .retry_strategy
                            .next_delay(Instant::now());
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

    use std::{pin::pin, sync::Arc, time::Duration};

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
        };

        let mut reconnecting_request = ReconnectingRequest::new(transport.clone(), req_props, None);

        // sets initial state with a failing request
        let resp = transport.request(http::Request::builder().uri(uri).body(None).unwrap());

        reconnecting_request.state = State::Connecting {
            retry: reconnecting_request.props.reconnect_opts.retry_initial,
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
}
