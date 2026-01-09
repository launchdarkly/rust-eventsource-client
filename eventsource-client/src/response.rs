use http::{HeaderMap, HeaderValue, StatusCode};

use crate::{ByteStream, HeaderError};

/// Represents an error response body as a stream of bytes.
///
/// The body is provided as a stream so that users can read error details if needed.
/// For large error responses, the stream allows processing without loading the entire
/// response into memory.
pub struct ErrorBody {
    body: ByteStream,
}

impl ErrorBody {
    /// Create a new ErrorBody from a ByteStream
    pub fn new(body: ByteStream) -> Self {
        Self { body }
    }

    /// Consume this ErrorBody and return the underlying ByteStream
    pub fn into_stream(self) -> ByteStream {
        self.body
    }
}

impl std::fmt::Debug for ErrorBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ErrorBody")
            .field("body", &"<stream>")
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Response {
    status_code: StatusCode,
    headers: HeaderMap<HeaderValue>,
}

impl Response {
    pub fn new(status_code: StatusCode, headers: HeaderMap<HeaderValue>) -> Self {
        Self {
            status_code,
            headers,
        }
    }

    /// Returns the status code of this response.
    pub fn status(&self) -> u16 {
        self.status_code.as_u16()
    }

    /// Returns the list of header keys present in this response.
    pub fn get_header_keys(&self) -> Vec<&str> {
        self.headers.keys().map(|key| key.as_str()).collect()
    }

    /// Returns the value of a header.
    ///
    /// If the header contains more than one value, only the first value is returned. Refer to
    /// [`get_header_values`] for a method that returns all values.
    pub fn get_header_value(&self, key: &str) -> std::result::Result<Option<&str>, HeaderError> {
        if let Some(value) = self.headers.get(key) {
            value
                .to_str()
                .map(Some)
                .map_err(|e| HeaderError::new(Box::new(e)))
        } else {
            Ok(None)
        }
    }

    /// Returns all values for a header.
    ///
    /// If the header contains only one value, it will be returned as a single-element vector.
    /// Refer to [`get_header_value`] for a method that returns only a single value.
    pub fn get_header_values(&self, key: &str) -> std::result::Result<Vec<&str>, HeaderError> {
        self.headers
            .get_all(key)
            .iter()
            .map(|value| value.to_str().map_err(|e| HeaderError::new(Box::new(e))))
            .collect()
    }
}
