use hyper::body::Buf;
use hyper::{header::HeaderValue, Body, HeaderMap, StatusCode};

use crate::{Error, HeaderError};

pub struct ErrorBody {
    body: Body,
}

impl ErrorBody {
    pub fn new(body: Body) -> Self {
        Self { body }
    }

    /// Returns the body of the response as a vector of bytes.
    ///
    /// Caution: This method reads the entire body into memory. You should only use this method if
    /// you know the response is of a reasonable size.
    pub async fn body_bytes(self) -> Result<Vec<u8>, Error> {
        let buf = match hyper::body::aggregate(self.body).await {
            Ok(buf) => buf,
            Err(err) => return Err(Error::HttpStream(Box::new(err))),
        };

        Ok(buf.chunk().to_vec())
    }
}

impl std::fmt::Debug for ErrorBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ErrorBody").finish()
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
