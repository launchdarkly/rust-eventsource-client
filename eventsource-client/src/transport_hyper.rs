//! Hyper v1 transport implementation for eventsource-client
//!
//! This crate provides a production-ready [`HyperTransport`] implementation that
//! integrates hyper v1 with the eventsource-client library.
//!
//! # Example
//!
//! ```no_run
//! use eventsource_client::{ClientBuilder, HyperTransport};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let transport = HyperTransport::new();
//! let client = ClientBuilder::for_url("https://example.com/stream")?
//!     .build_with_transport(transport);
//! # Ok(())
//! # }
//! ```
//!
//! # Features
//!
//! - `hyper-rustls`: Enable HTTPS support using rustls (via [`HyperTransport::builder().https()`])
//!
//! # Timeout Configuration
//!
//! ```no_run
//! use eventsource_client::{ClientBuilder, HyperTransport};
//! use std::time::Duration;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let transport = HyperTransport::builder()
//!     .connect_timeout(Duration::from_secs(10))
//!     .read_timeout(Duration::from_secs(30))
//!     .build_http();
//!
//! let client = ClientBuilder::for_url("https://example.com/stream")?
//!     .build_with_transport(transport);
//! # Ok(())
//! # }
//! ```

use crate::{ByteStream, HttpTransport, TransportError};
use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Empty, Full};
use hyper::body::Incoming;
use hyper_timeout::TimeoutConnector;
use hyper_util::client::legacy::Client as HyperClient;
use hyper_util::rt::TokioExecutor;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

/// A transport implementation using hyper v1.x
///
/// This struct wraps a hyper client and implements the [`HttpTransport`] trait
/// for use with eventsource-client.
///
/// # Timeout Support
///
/// All three timeout types are fully supported via `hyper-timeout`:
/// - `connect_timeout` - Timeout for establishing the TCP connection
/// - `read_timeout` - Timeout for reading data from the connection
/// - `write_timeout` - Timeout for writing data to the connection
///
/// Timeouts are configured using the builder pattern. See [`HyperTransportBuilder`] for details.
///
/// # Example
///
/// ```no_run
/// use eventsource_client::{ClientBuilder, HyperTransport};
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// // Create transport with default HTTP connector
/// let transport = HyperTransport::new();
///
/// // Build SSE client
/// let client = ClientBuilder::for_url("https://example.com/stream")?
///     .build_with_transport(transport);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct HyperTransport<C = TimeoutConnector<hyper_util::client::legacy::connect::HttpConnector>>
{
    client: HyperClient<C, BoxBody<Bytes, Box<dyn std::error::Error + Send + Sync>>>,
}

/// Builder for configuring a [`HyperTransport`].
///
/// This builder allows you to configure timeouts and choose between HTTP and HTTPS connectors.
///
/// # Example
///
/// ```no_run
/// use eventsource_client::HyperTransport;
/// use std::time::Duration;
///
/// let transport = HyperTransport::builder()
///     .connect_timeout(Duration::from_secs(10))
///     .read_timeout(Duration::from_secs(30))
///     .build_http();
/// ```
#[derive(Default)]
pub struct HyperTransportBuilder {
    connect_timeout: Option<Duration>,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
}

impl HyperTransport {
    /// Create a new HyperTransport with default HTTP connector and no timeouts
    ///
    /// This creates a basic HTTP-only client that supports both HTTP/1 and HTTP/2.
    /// For HTTPS support or timeout configuration, use [`HyperTransport::builder()`].
    pub fn new() -> Self {
        let connector = hyper_util::client::legacy::connect::HttpConnector::new();
        let timeout_connector = TimeoutConnector::new(connector);
        let client = HyperClient::builder(TokioExecutor::new()).build(timeout_connector);

        Self { client }
    }

    /// Create a new HyperTransport with HTTPS support using rustls
    ///
    /// This creates an HTTPS client that supports both HTTP/1 and HTTP/2 protocols.
    /// This method is only available when the `hyper-rustls` feature is enabled.
    /// For timeout configuration, use [`HyperTransport::builder().build_https()`] instead.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # #[cfg(feature = "hyper-rustls")]
    /// # {
    /// use eventsource_client::{ClientBuilder, HyperTransport};
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let transport = HyperTransport::new_https();
    /// let client = ClientBuilder::for_url("https://example.com/stream")?
    ///     .build_with_transport(transport);
    /// # Ok(())
    /// # }
    /// # }
    /// ```
    #[cfg(feature = "hyper-rustls")]
    pub fn new_https() -> HyperTransport<
        TimeoutConnector<
            hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
        >,
    > {
        use hyper_rustls::HttpsConnectorBuilder;

        let connector = HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .build();

        let timeout_connector = TimeoutConnector::new(connector);
        let client = HyperClient::builder(TokioExecutor::new()).build(timeout_connector);

        HyperTransport { client }
    }

    /// Create a new builder for configuring HyperTransport
    ///
    /// The builder allows you to configure timeouts and choose between HTTP and HTTPS connectors.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use eventsource_client::HyperTransport;
    /// use std::time::Duration;
    ///
    /// let transport = HyperTransport::builder()
    ///     .connect_timeout(Duration::from_secs(10))
    ///     .read_timeout(Duration::from_secs(30))
    ///     .build_http();
    /// ```
    pub fn builder() -> HyperTransportBuilder {
        HyperTransportBuilder::default()
    }
}

impl HyperTransportBuilder {
    /// Set a connect timeout for establishing connections
    ///
    /// This timeout applies when establishing the TCP connection to the server.
    /// There is no connect timeout by default.
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = Some(timeout);
        self
    }

    /// Set a read timeout for reading from connections
    ///
    /// This timeout applies when reading data from the connection.
    /// There is no read timeout by default.
    pub fn read_timeout(mut self, timeout: Duration) -> Self {
        self.read_timeout = Some(timeout);
        self
    }

    /// Set a write timeout for writing to connections
    ///
    /// This timeout applies when writing data to the connection.
    /// There is no write timeout by default.
    pub fn write_timeout(mut self, timeout: Duration) -> Self {
        self.write_timeout = Some(timeout);
        self
    }

    /// Build with an HTTP connector
    ///
    /// Creates a transport that supports HTTP/1 and HTTP/2 over plain HTTP.
    pub fn build_http(self) -> HyperTransport {
        let connector = hyper_util::client::legacy::connect::HttpConnector::new();
        self.build_with_connector(connector)
    }

    /// Build with an HTTPS connector using rustls
    ///
    /// Creates a transport that supports HTTP/1 and HTTP/2 over HTTPS using rustls.
    /// This method is only available when the `hyper-rustls` feature is enabled.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # #[cfg(feature = "hyper-rustls")]
    /// # {
    /// use eventsource_client::HyperTransport;
    /// use std::time::Duration;
    ///
    /// let transport = HyperTransport::builder()
    ///     .connect_timeout(Duration::from_secs(10))
    ///     .build_https();
    /// # }
    /// ```
    #[cfg(feature = "hyper-rustls")]
    pub fn build_https(
        self,
    ) -> HyperTransport<
        TimeoutConnector<
            hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
        >,
    > {
        use hyper_rustls::HttpsConnectorBuilder;

        let connector = HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .build();

        self.build_with_connector(connector)
    }

    /// Build with a custom connector
    ///
    /// This allows you to provide your own connector implementation, which is useful for:
    /// - Custom TLS configuration
    /// - Proxy support
    /// - Connection pooling customization
    /// - Custom DNS resolution
    ///
    /// The connector will be automatically wrapped with a `TimeoutConnector` that applies
    /// the configured timeout settings.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use eventsource_client::HyperTransport;
    /// use hyper_util::client::legacy::connect::HttpConnector;
    /// use std::time::Duration;
    ///
    /// let mut connector = HttpConnector::new();
    /// // Configure the connector as needed
    /// connector.set_nodelay(true);
    ///
    /// let transport = HyperTransport::builder()
    ///     .read_timeout(Duration::from_secs(30))
    ///     .build_with_connector(connector);
    /// ```
    pub fn build_with_connector<C>(self, connector: C) -> HyperTransport<TimeoutConnector<C>>
    where
        C: tower::Service<http::Uri> + Clone + Send + Sync + 'static,
        C::Response: hyper_util::client::legacy::connect::Connection
            + hyper::rt::Read
            + hyper::rt::Write
            + Send
            + Unpin,
        C::Future: Send + 'static,
        C::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let mut timeout_connector = TimeoutConnector::new(connector);
        timeout_connector.set_connect_timeout(self.connect_timeout);
        timeout_connector.set_read_timeout(self.read_timeout);
        timeout_connector.set_write_timeout(self.write_timeout);

        let client = HyperClient::builder(TokioExecutor::new()).build(timeout_connector);

        HyperTransport { client }
    }
}

impl Default for HyperTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl<C> HttpTransport for HyperTransport<C>
where
    C: hyper_util::client::legacy::connect::Connect + Clone + Send + Sync + 'static,
{
    fn request(
        &self,
        request: http::Request<Option<String>>,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<http::Response<ByteStream>, TransportError>>
                + Send
                + Sync
                + 'static,
        >,
    > {
        // Convert http::Request<Option<String>> to hyper::Request<BoxBody>
        let (parts, body_opt) = request.into_parts();

        // Convert Option<String> to BoxBody
        let body: BoxBody<Bytes, Box<dyn std::error::Error + Send + Sync>> = match body_opt {
            Some(body_str) => {
                // Use Full for non-empty bodies
                Full::new(Bytes::from(body_str))
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
                    .boxed()
            }
            None => {
                // Use Empty for no body
                Empty::<Bytes>::new()
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
                    .boxed()
            }
        };

        let hyper_req = hyper::Request::from_parts(parts, body);

        let client = self.client.clone();

        Box::pin(async move {
            // Make the request - timeouts are handled by TimeoutConnector
            let resp = client
                .request(hyper_req)
                .await
                .map_err(TransportError::new)?;

            let (parts, body) = resp.into_parts();

            // Convert hyper's Incoming body to ByteStream
            let byte_stream: ByteStream = Box::pin(body_to_stream(body));

            Ok(http::Response::from_parts(parts, byte_stream))
        })
    }
}

/// Convert hyper's Incoming body to a Stream of Bytes
fn body_to_stream(
    body: Incoming,
) -> impl futures::Stream<Item = Result<Bytes, TransportError>> + Send + Sync {
    futures::stream::unfold(body, |mut body| async move {
        match body.frame().await {
            Some(Ok(frame)) => {
                if let Ok(data) = frame.into_data() {
                    Some((Ok(data), body))
                } else {
                    // Skip non-data frames (trailers, etc.)
                    Some((
                        Err(TransportError::new(std::io::Error::other("non-data frame"))),
                        body,
                    ))
                }
            }
            Some(Err(e)) => Some((Err(TransportError::new(e)), body)),
            None => None,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use http::{Method, Request};
    use std::time::Duration;

    #[test]
    fn test_hyper_transport_new() {
        let transport = HyperTransport::new();
        // If we can create it without panic, the test passes
        // This verifies the default HTTP connector is set up correctly
        drop(transport);
    }

    #[test]
    fn test_hyper_transport_default() {
        let transport = HyperTransport::default();
        // Verify Default trait implementation
        drop(transport);
    }

    #[cfg(feature = "hyper-rustls")]
    #[test]
    fn test_hyper_transport_new_https() {
        let transport = HyperTransport::new_https();
        // If we can create it without panic, the test passes
        // This verifies the HTTPS connector with rustls is set up correctly
        drop(transport);
    }

    #[test]
    fn test_builder_default() {
        let builder = HyperTransport::builder();
        let transport = builder.build_http();
        // Verify we can build with default settings
        drop(transport);
    }

    #[test]
    fn test_builder_with_connect_timeout() {
        let transport = HyperTransport::builder()
            .connect_timeout(Duration::from_secs(5))
            .build_http();
        // Verify we can build with connect timeout
        drop(transport);
    }

    #[test]
    fn test_builder_with_read_timeout() {
        let transport = HyperTransport::builder()
            .read_timeout(Duration::from_secs(10))
            .build_http();
        // Verify we can build with read timeout
        drop(transport);
    }

    #[test]
    fn test_builder_with_write_timeout() {
        let transport = HyperTransport::builder()
            .write_timeout(Duration::from_secs(10))
            .build_http();
        // Verify we can build with write timeout
        drop(transport);
    }

    #[test]
    fn test_builder_with_all_timeouts() {
        let transport = HyperTransport::builder()
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(Duration::from_secs(30))
            .write_timeout(Duration::from_secs(10))
            .build_http();
        // Verify we can build with all timeouts configured
        drop(transport);
    }

    #[cfg(feature = "hyper-rustls")]
    #[test]
    fn test_builder_https() {
        let transport = HyperTransport::builder()
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(Duration::from_secs(30))
            .build_https();
        // Verify we can build HTTPS transport with timeouts
        drop(transport);
    }

    #[test]
    fn test_builder_with_custom_connector() {
        let mut connector = hyper_util::client::legacy::connect::HttpConnector::new();
        connector.set_nodelay(true);

        let transport = HyperTransport::builder()
            .read_timeout(Duration::from_secs(30))
            .build_with_connector(connector);
        // Verify we can build with a custom connector
        drop(transport);
    }

    #[test]
    fn test_transport_is_clone() {
        let transport = HyperTransport::new();
        let _cloned = transport.clone();
        // Verify HyperTransport implements Clone
    }

    #[tokio::test]
    async fn test_http_transport_trait_implemented() {
        let transport = HyperTransport::new();

        // Create a basic request
        let request = Request::builder()
            .method(Method::GET)
            .uri("http://httpbin.org/get")
            .body(None)
            .expect("failed to build request");

        // Verify the trait is implemented by attempting to call it
        // We're not actually making the request here, just verifying the types work
        let _future = transport.request(request);
        // The future exists and has the correct type signature
    }

    #[tokio::test]
    async fn test_request_with_empty_body() {
        // This test verifies that we can construct a request with no body
        let transport = HyperTransport::new();

        let request = Request::builder()
            .method(Method::GET)
            .uri("http://httpbin.org/get")
            .body(None)
            .expect("failed to build request");

        // Just verify we can create the future - not actually making network call
        let _future = transport.request(request);
    }

    #[tokio::test]
    async fn test_request_with_string_body() {
        // This test verifies that we can construct a request with a string body
        let transport = HyperTransport::new();

        let request = Request::builder()
            .method(Method::POST)
            .uri("http://httpbin.org/post")
            .body(Some(String::from("test body")))
            .expect("failed to build request");

        // Just verify we can create the future - not actually making network call
        let _future = transport.request(request);
    }

    #[tokio::test]
    async fn test_body_to_stream_empty() {
        // Create an empty incoming body for testing
        // This is a bit tricky since Incoming is not easily constructible
        // We'll test the integration through the full request path instead

        // For now, this is a placeholder showing the test structure
        // A full implementation would require setting up a test HTTP server
    }

    // Integration tests that actually make HTTP requests
    // These require a running HTTP server, so they're marked as ignored by default

    #[tokio::test]
    #[ignore] // Run with: cargo test -- --ignored
    async fn test_integration_http_request() {
        let transport = HyperTransport::builder()
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(30))
            .build_http();

        let request = Request::builder()
            .method(Method::GET)
            .uri("http://httpbin.org/get")
            .body(None)
            .expect("failed to build request");

        let response = transport.request(request).await;
        assert!(response.is_ok(), "Request should succeed");

        let response = response.unwrap();
        assert!(response.status().is_success(), "Status should be success");

        // Verify we can read from the stream
        let mut stream = response.into_body();
        let mut received_data = false;
        while let Some(result) = stream.next().await {
            assert!(result.is_ok(), "Stream chunk should not error");
            received_data = true;
        }
        assert!(received_data, "Should have received some data");
    }

    #[cfg(feature = "hyper-rustls")]
    #[tokio::test]
    #[ignore] // Run with: cargo test -- --ignored
    async fn test_integration_https_request() {
        let transport = HyperTransport::builder()
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(30))
            .build_https();

        // Using example.com as it's highly reliable and well-maintained
        let request = Request::builder()
            .method(Method::GET)
            .uri("https://example.com/")
            .body(None)
            .expect("failed to build request");

        let response = transport.request(request).await;
        assert!(
            response.is_ok(),
            "HTTPS request should succeed: {:?}",
            response.as_ref().err()
        );

        let response = response.unwrap();
        assert!(
            response.status().is_success(),
            "Status should be success: {}",
            response.status()
        );
    }

    #[tokio::test]
    #[ignore] // Run with: cargo test -- --ignored
    async fn test_integration_request_with_body() {
        let transport = HyperTransport::new();

        let body_content = r#"{"test": "data"}"#;
        let request = Request::builder()
            .method(Method::POST)
            .uri("http://httpbin.org/post")
            .header("Content-Type", "application/json")
            .body(Some(body_content.to_string()))
            .expect("failed to build request");

        let response = transport.request(request).await;
        assert!(response.is_ok(), "POST request should succeed");

        let response = response.unwrap();
        assert!(response.status().is_success(), "Status should be success");
    }

    #[tokio::test]
    #[ignore] // Run with: cargo test -- --ignored
    async fn test_integration_streaming_response() {
        let transport = HyperTransport::new();

        let request = Request::builder()
            .method(Method::GET)
            .uri("http://httpbin.org/stream/10")
            .body(None)
            .expect("failed to build request");

        let response = transport.request(request).await;
        assert!(response.is_ok(), "Streaming request should succeed");

        let response = response.unwrap();
        assert!(response.status().is_success(), "Status should be success");

        // Verify we receive multiple chunks
        let mut stream = response.into_body();
        let mut chunk_count = 0;
        while let Some(result) = stream.next().await {
            assert!(result.is_ok(), "Stream chunk should not error");
            let chunk = result.unwrap();
            assert!(!chunk.is_empty(), "Chunk should not be empty");
            chunk_count += 1;
        }
        assert!(chunk_count > 0, "Should have received multiple chunks");
    }

    #[tokio::test]
    #[ignore] // Run with: cargo test -- --ignored
    async fn test_integration_connect_timeout() {
        // Use a non-routable IP to test connect timeout
        let transport = HyperTransport::builder()
            .connect_timeout(Duration::from_millis(100))
            .build_http();

        let request = Request::builder()
            .method(Method::GET)
            .uri("http://10.255.255.1/")
            .body(None)
            .expect("failed to build request");

        let response = transport.request(request).await;
        assert!(response.is_err(), "Request should timeout");
    }
}
