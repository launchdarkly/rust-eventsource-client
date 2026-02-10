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
//! let transport = HyperTransport::new()?;
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
//!     .build_http()?;
//!
//! let client = ClientBuilder::for_url("https://example.com/stream")?
//!     .build_with_transport(transport);
//! # Ok(())
//! # }
//! ```

use crate::{ByteStream, HttpTransport, TransportError};
use bytes::Bytes;
use http::Uri;
use http_body_util::{combinators::BoxBody, BodyExt, Empty, Full};
use hyper::body::Incoming;
use hyper_http_proxy::{Intercept, Proxy, ProxyConnector};
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
/// let transport = HyperTransport::new()?;
///
/// // Build SSE client
/// let client = ClientBuilder::for_url("https://example.com/stream")?
///     .build_with_transport(transport);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct HyperTransport<
    C = ProxyConnector<TimeoutConnector<hyper_util::client::legacy::connect::HttpConnector>>,
> {
    client: HyperClient<C, BoxBody<Bytes, Box<dyn std::error::Error + Send + Sync>>>,
}

impl HyperTransport {
    /// Create a new HyperTransport with default HTTP connector and no timeouts
    ///
    /// This creates a basic HTTP-only client that supports both HTTP/1 and HTTP/2.
    /// For HTTPS support or timeout configuration, use [`HyperTransport::builder()`].
    pub fn new() -> Result<Self, std::io::Error> {
        let connector = hyper_util::client::legacy::connect::HttpConnector::new();
        let timeout_connector = TimeoutConnector::new(connector);
        let proxy_connector = ProxyConnector::new(timeout_connector)?;
        let client = HyperClient::builder(TokioExecutor::new()).build(proxy_connector);

        Ok(Self { client })
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
    /// let transport = HyperTransport::new_https()?;
    /// let client = ClientBuilder::for_url("https://example.com/stream")?
    ///     .build_with_transport(transport);
    /// # Ok(())
    /// # }
    /// # }
    /// ```
    #[cfg(feature = "hyper-rustls")]
    pub fn new_https() -> Result<
        HyperTransport<
            ProxyConnector<
                TimeoutConnector<
                    hyper_rustls::HttpsConnector<
                        hyper_util::client::legacy::connect::HttpConnector,
                    >,
                >,
            >,
        >,
        std::io::Error,
    > {
        HyperTransport::builder().build_https()
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
    proxy_config: Option<ProxyConfig>,
}

impl HyperTransportBuilder {
    pub fn disable_proxy(mut self) -> Self {
        self.proxy_config = Some(ProxyConfig::Disabled);
        self
    }

    /// Configure the transport to automatically detect proxy settings from environment variables.
    /// This is the default behavior if no proxy configuration method is called.
    ///
    /// The transport will check `HTTP_PROXY`, `HTTPS_PROXY`, and `NO_PROXY` environment variables to determine proxy settings.
    /// Uppercase variants take precedence over lowercase.
    ///
    /// `NO_PROXY` is respected to bypass the proxy for specified hosts.
    ///
    /// If both `HTTP_PROXY` and `HTTPS_PROXY` are set, the transport will route requests based on the scheme (http vs https).
    /// If only `HTTP_PROXY` is set, all requests will route through that proxy regardless of scheme.
    /// If neither is set, no proxy will be used.
    pub fn auto_proxy(mut self) -> Self {
        self.proxy_config = Some(ProxyConfig::Auto);
        self
    }

    /// Configure the transport to use a custom proxy URL for all requests The URL should include
    /// the scheme (http:// or https://) and can optionally include authentication info.
    ///
    /// When this is set, the transport will route all requests through the specified proxy,
    /// regardless of environment variables.
    pub fn proxy_url(mut self, proxy_url: String) -> Self {
        self.proxy_config = Some(ProxyConfig::Custom(proxy_url));
        self
    }

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
    pub fn build_http(self) -> Result<HyperTransport, std::io::Error> {
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
    ///     .build_https()
    ///     .expect("failed to build HTTPS transport");
    /// # }
    /// ```
    #[cfg(feature = "hyper-rustls")]
    pub fn build_https(
        self,
    ) -> Result<
        HyperTransport<
            ProxyConnector<
                TimeoutConnector<
                    hyper_rustls::HttpsConnector<
                        hyper_util::client::legacy::connect::HttpConnector,
                    >,
                >,
            >,
        >,
        std::io::Error,
    > {
        use hyper_rustls::HttpsConnectorBuilder;

        let connector = HttpsConnectorBuilder::new()
            .with_native_roots()
            .unwrap_or_else(|_| {
                log::debug!("Falling back to webpki roots for HTTPS connector");
                HttpsConnectorBuilder::new().with_webpki_roots()
            })
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
    pub fn build_with_connector<C>(
        self,
        connector: C,
    ) -> Result<HyperTransport<ProxyConnector<TimeoutConnector<C>>>, std::io::Error>
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

        let mut proxy_connector = ProxyConnector::new(timeout_connector)?;

        match self.proxy_config {
            Some(ProxyConfig::Auto) | None => {
                let http_proxy = std::env::var("http_proxy")
                    .or_else(|_| std::env::var("HTTP_PROXY"))
                    .unwrap_or_default();
                let https_proxy = std::env::var("https_proxy")
                    .or_else(|_| std::env::var("HTTPS_PROXY"))
                    .unwrap_or_default();
                let no_proxy = NoProxyMatcher::from_env();

                if !https_proxy.is_empty() {
                    let https_uri = https_proxy
                        .parse::<Uri>()
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
                    let no_proxy = no_proxy.clone();
                    let custom: Intercept = Intercept::Custom(
                        (move |schema: Option<&str>,
                               host: Option<&str>,
                               _port: Option<u16>|
                              -> bool {
                            // This function should only enforce validation when it matches
                            // the schema of the proxy.
                            if !matches!(schema, Some("https")) {
                                return false;
                            }

                            match host {
                                None => false,
                                Some(h) => !no_proxy.matches(h),
                            }
                        })
                        .into(),
                    );
                    let proxy = Proxy::new(custom, https_uri);
                    proxy_connector.add_proxy(proxy);
                }

                if !http_proxy.is_empty() {
                    let http_uri = http_proxy
                        .parse::<Uri>()
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
                    // If http_proxy is set but https_proxy is not, then all hosts are eligible to
                    // route through the http_proxy.
                    let proxy_all = https_proxy.is_empty();
                    let custom: Intercept = Intercept::Custom(
                        (move |schema: Option<&str>,
                               host: Option<&str>,
                               _port: Option<u16>|
                              -> bool {
                            if !proxy_all && matches!(schema, Some("https")) {
                                return false;
                            }

                            match host {
                                None => false,
                                Some(h) => !no_proxy.matches(h),
                            }
                        })
                        .into(),
                    );
                    let proxy = Proxy::new(custom, http_uri);
                    proxy_connector.add_proxy(proxy);
                }
            }
            Some(ProxyConfig::Disabled) => {
                // No proxies will be added, so the client will connect directly
            }
            Some(ProxyConfig::Custom(url)) => {
                let uri = url
                    .parse::<Uri>()
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
                proxy_connector.add_proxy(Proxy::new(Intercept::All, uri));
            }
        };

        let client = HyperClient::builder(TokioExecutor::new()).build(proxy_connector);

        Ok(HyperTransport { client })
    }
}

/// Proxy configuration for HyperTransport.
///
/// This determines whether and how the transport uses an HTTP/HTTPS proxy.
#[derive(Debug, Clone)]
enum ProxyConfig {
    /// Automatically detect proxy from environment variables (default).
    ///
    /// Checks `HTTP_PROXY`, `HTTPS_PROXY`, and `NO_PROXY` environment variables.
    /// Uppercase variants take precedence over lowercase.
    Auto,

    /// Explicitly disable proxy support.
    ///
    /// No proxy will be used even if environment variables are set.
    Disabled,

    /// Use a custom proxy URL.
    ///
    /// Format: `http://[user:pass@]host:port`
    Custom(String),
}

#[allow(clippy::derivable_impls)]
impl Default for ProxyConfig {
    fn default() -> Self {
        ProxyConfig::Auto
    }
}

#[derive(Debug, Clone)]
struct NoProxyMatcher {
    // The combination of match_all and parts determines whether we should match all, match none,
    // or match some.
    //
    // If match_all is true, we match all hosts (i.e., no_proxy includes "*")
    // If match_all is false and parts is None, we match no hosts (i.e., no_proxy is empty).
    // If match_all is false and parts is Some, we match based on the parts.
    match_all: bool,
    parts: Option<Vec<String>>,
}

impl NoProxyMatcher {
    fn from_env() -> Self {
        let no_proxy = std::env::var("no_proxy")
            .or_else(|_| std::env::var("NO_PROXY"))
            .unwrap_or_default();

        if no_proxy.is_empty() {
            return Self::default();
        }

        let parts: Vec<String> = no_proxy
            .split(',')
            .map(|part| part.trim())
            .filter(|part| !part.is_empty())
            .map(String::from)
            .collect();

        let match_all = parts.iter().any(|part| part == "*");

        Self {
            match_all,
            parts: if match_all { None } else { Some(parts) },
        }
    }

    /// Check if the given host matches the no_proxy rules. If this function returns true, the
    /// proxy should be bypassed.
    fn matches(&self, host: &str) -> bool {
        if self.match_all {
            return true;
        }

        if let Some(parts) = &self.parts {
            for part in parts {
                if host.ends_with(part) {
                    return true;
                }

                if let Some(trimmed) = part.strip_prefix('.') {
                    if host == trimmed {
                        return true;
                    }
                }
            }
        }

        false
    }
}

impl Default for NoProxyMatcher {
    /// The default configuration matches nothing.
    fn default() -> Self {
        Self {
            match_all: false,
            parts: None,
        }
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

    #[cfg(feature = "hyper-rustls")]
    #[test]
    fn test_hyper_transport_new_https() {
        let transport = HyperTransport::new_https().expect("transport failed to build");
        // If we can create it without panic, the test passes
        // This verifies the HTTPS connector with rustls is set up correctly
        drop(transport);
    }

    #[test]
    fn test_builder_default() {
        let builder = HyperTransport::builder();
        let transport = builder.build_http().expect("failed to build transport");
        // Verify we can build with default settings
        drop(transport);
    }

    #[test]
    fn test_builder_with_connect_timeout() {
        let transport = HyperTransport::builder()
            .connect_timeout(Duration::from_secs(5))
            .build_http()
            .expect("failed to build transport");
        // Verify we can build with connect timeout
        drop(transport);
    }

    #[test]
    fn test_builder_with_read_timeout() {
        let transport = HyperTransport::builder()
            .read_timeout(Duration::from_secs(10))
            .build_http()
            .expect("failed to build transport");
        // Verify we can build with read timeout
        drop(transport);
    }

    #[test]
    fn test_builder_with_write_timeout() {
        let transport = HyperTransport::builder()
            .write_timeout(Duration::from_secs(10))
            .build_http()
            .expect("failed to build transport");
        // Verify we can build with write timeout
        drop(transport);
    }

    #[test]
    fn test_builder_with_all_timeouts() {
        let transport = HyperTransport::builder()
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(Duration::from_secs(30))
            .write_timeout(Duration::from_secs(10))
            .build_http()
            .expect("failed to build transport");
        // Verify we can build with all timeouts configured
        drop(transport);
    }

    #[cfg(feature = "hyper-rustls")]
    #[test]
    fn test_builder_https() {
        let transport = HyperTransport::builder()
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(Duration::from_secs(30))
            .build_https()
            .expect("failed to build HTTPS transport");
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
        let transport = HyperTransport::new().expect("failed to build transport");
        let _cloned = transport.clone();
        // Verify HyperTransport implements Clone
    }

    #[tokio::test]
    async fn test_http_transport_trait_implemented() {
        let transport = HyperTransport::new().expect("failed to build transport");

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
        let transport = HyperTransport::new().expect("failed to build transport");

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
        let transport = HyperTransport::new().expect("failed to build transport");

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
            .build_http()
            .expect("failed to build transport");

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
            .build_https()
            .expect("failed to build HTTPS transport");

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
        let transport = HyperTransport::new().expect("failed to build transport");

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
        let transport = HyperTransport::new().expect("failed to build transport");

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
            .build_http()
            .expect("failed to build transport");

        let request = Request::builder()
            .method(Method::GET)
            .uri("http://10.255.255.1/")
            .body(None)
            .expect("failed to build request");

        let response = transport.request(request).await;
        assert!(response.is_err(), "Request should timeout");
    }
}
