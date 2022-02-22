use hyper::StatusCode;

/// Error type returned from this library's functions.
#[derive(Debug)]
pub enum Error {
    TimedOut,
    StreamClosed,
    /// The HTTP request failed.
    HttpRequest(StatusCode),
    /// An error reading from the HTTP response body.
    HttpStream(Box<dyn std::error::Error + Send + 'static>),
    /// The HTTP response stream ended
    Eof,
    /// The HTTP response stream ended unexpectedly (e.g. in the
    /// middle of an event).
    UnexpectedEof,
    /// Encountered a line not conforming to the SSE protocol.
    InvalidLine(String),
    /// Encountered an event type that is not a valid UTF-8 byte sequence.
    InvalidEventType(std::str::Utf8Error),
    InvalidEvent,
    /// An unexpected failure occurred.
    Unexpected(Box<dyn std::error::Error + Send + 'static>),
}

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
            Error::Unexpected(err) => Some(err.as_ref()),
            _ => None,
        }
    }
}

impl<E> From<E> for Error
where
    E: std::error::Error + Send + 'static,
{
    fn from(e: E) -> Error {
        Error::Unexpected(Box::new(e))
    }
}

pub type Result<T> = std::result::Result<T, Error>;
