//! HTTP transport abstraction for Server-Sent Events client.
//!
//! This module defines the [`HttpTransport`] trait which allows users to plug in
//! their own HTTP client implementation (hyper, reqwest, or custom).
//!
//! # Example
//!
//! See the `examples/` directory for reference implementations using popular HTTP clients.

use bytes::Bytes;
use futures::Stream;
use std::error::Error as StdError;
use std::fmt;
use std::future::Future;
use std::pin::Pin;

// Re-export http crate types for convenience
pub use http::{HeaderMap, HeaderValue, Request, Response, StatusCode, Uri};

/// A pinned, boxed stream of bytes returned by HTTP transports.
///
/// This represents the streaming response body from an HTTP request.
pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, TransportError>> + Send + Sync>>;

/// A pinned, boxed future for an HTTP response.
///
/// This represents the future returned by [`HttpTransport::request`].
pub type ResponseFuture = Pin<
    Box<
        dyn Future<Output = Result<Response<ByteStream>, TransportError>> + Send + Sync,
    >,
>;

/// Error type for HTTP transport operations.
///
/// This wraps transport-specific errors (network failures, timeouts, etc.) in a
/// common error type that the SSE client can handle uniformly.
#[derive(Debug)]
pub struct TransportError {
    inner: Box<dyn StdError + Send + Sync + 'static>,
}

impl TransportError {
    /// Create a new transport error from any error type.
    pub fn new(err: impl StdError + Send + Sync + 'static) -> Self {
        Self {
            inner: Box::new(err),
        }
    }

    /// Get a reference to the inner error.
    pub fn inner(&self) -> &(dyn StdError + Send + Sync + 'static) {
        &*self.inner
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "transport error: {}", self.inner)
    }
}

impl StdError for TransportError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(&*self.inner)
    }
}

/// Trait for pluggable HTTP transport implementations.
///
/// Implement this trait to provide HTTP request/response functionality for the
/// SSE client. The transport is responsible for:
/// - Establishing HTTP connections (with TLS if needed)
/// - Sending HTTP requests
/// - Returning streaming HTTP responses
/// - Handling timeouts (if desired)
///
/// # Example
///
/// ```ignore
/// use eventsource_client::{HttpTransport, ByteStream, TransportError};
/// use std::pin::Pin;
/// use std::future::Future;
///
/// struct MyTransport {
///     // Your HTTP client here
/// }
///
/// impl HttpTransport for MyTransport {
///     fn request(
///         &self,
///         request: http::Request<Option<String>>,
///     ) -> Pin<Box<dyn Future<Output = Result<http::Response<ByteStream>, TransportError>> + Send>> {
///         // Extract body from request
///         // Convert request to your HTTP client's format
///         // Make the request
///         // Return streaming response
///         todo!()
///     }
/// }
/// ```
pub trait HttpTransport: Send + Sync + 'static {
    /// Execute an HTTP request and return a streaming response.
    ///
    /// # Arguments
    ///
    /// * `request` - The HTTP request to execute. The body type is `Option<String>`
    ///   to support methods like REPORT that may include a request body. Most SSE
    ///   requests use GET and will have `None` as the body.
    ///
    /// # Returns
    ///
    /// A future that resolves to an HTTP response with a streaming body, or a
    /// transport error if the request fails.
    ///
    /// The response should include:
    /// - Status code
    /// - Response headers
    /// - A stream of body bytes
    ///
    /// # Notes
    ///
    /// - The transport should NOT follow redirects - the SSE client handles this
    /// - The transport should NOT retry requests - the SSE client handles this
    /// - The transport MAY implement timeouts as desired
    fn request(&self, request: Request<Option<String>>) -> ResponseFuture;
}
