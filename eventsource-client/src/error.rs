use std::collections::HashMap;

use hyper::{body::Buf, Body, Response};

pub struct ResponseWrapper {
    response: Response<Body>,
}

impl ResponseWrapper {
    pub fn new(response: Response<Body>) -> Self {
        Self { response }
    }
    pub fn status(&self) -> u16 {
        self.response.status().as_u16()
    }
    pub fn headers(&self) -> Result<HashMap<&str, &str>> {
        let headers = self.response.headers();
        let mut map = HashMap::new();
        for (key, value) in headers.iter() {
            let key = key.as_str();
            let value = match value.to_str() {
                Ok(value) => value,
                Err(err) => return Err(Error::InvalidParameter(Box::new(err))),
            };
            map.insert(key, value);
        }
        Ok(map)
    }

    pub async fn body_bytes(self) -> Result<Vec<u8>> {
        let body = self.response.into_body();

        let buf = match hyper::body::aggregate(body).await {
            Ok(buf) => buf,
            Err(err) => return Err(Error::HttpStream(Box::new(err))),
        };

        Ok(buf.chunk().to_vec())
    }
}

impl std::fmt::Debug for ResponseWrapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResponseWrapper")
            .field("status", &self.status())
            .finish()
    }
}

/// Error type returned from this library's functions.
#[derive(Debug)]
pub enum Error {
    TimedOut,
    StreamClosed,
    /// An invalid request parameter
    InvalidParameter(Box<dyn std::error::Error + Send + Sync + 'static>),
    /// The HTTP response could not be handled.
    UnexpectedResponse(ResponseWrapper),
    /// An error reading from the HTTP response body.
    HttpStream(Box<dyn std::error::Error + Send + Sync + 'static>),
    /// The HTTP response stream ended
    Eof,
    /// The HTTP response stream ended unexpectedly (e.g. in the
    /// middle of an event).
    UnexpectedEof,
    /// Encountered a line not conforming to the SSE protocol.
    InvalidLine(String),
    InvalidEvent,
    /// Encountered a malformed Location header.
    MalformedLocationHeader(Box<dyn std::error::Error + Send + Sync + 'static>),
    /// Reached maximum redirect limit after encountering Location headers.
    MaxRedirectLimitReached(u32),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use Error::*;
        match self {
            TimedOut => write!(f, "timed out"),
            StreamClosed => write!(f, "stream closed"),
            InvalidParameter(err) => write!(f, "invalid parameter: {err}"),
            UnexpectedResponse(r) => {
                let status = r.status();
                write!(f, "unexpected response: {status}")
            }
            HttpStream(err) => write!(f, "http error: {err}"),
            Eof => write!(f, "eof"),
            UnexpectedEof => write!(f, "unexpected eof"),
            InvalidLine(line) => write!(f, "invalid line: {line}"),
            InvalidEvent => write!(f, "invalid event"),
            MalformedLocationHeader(err) => write!(f, "malformed header: {err}"),
            MaxRedirectLimitReached(limit) => write!(f, "maximum redirect limit reached: {limit}"),
        }
    }
}

impl std::error::Error for Error {}

impl PartialEq<Error> for Error {
    fn eq(&self, other: &Error) -> bool {
        use Error::*;
        if let (InvalidLine(msg1), InvalidLine(msg2)) = (self, other) {
            return msg1 == msg2;
        } else if let (UnexpectedEof, UnexpectedEof) = (self, other) {
            return true;
        }
        false
    }
}

impl Error {
    pub fn is_http_stream_error(&self) -> bool {
        if let Error::HttpStream(_) = self {
            return true;
        }
        false
    }

    pub fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::HttpStream(err) => Some(err.as_ref()),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
