use std::collections::BTreeMap as Map;
use std::str::from_utf8;

use futures::future::{self, Future};
use futures::stream::Stream;
use reqwest as r;
use reqwest::{r#async as ra, Url};

/*
 * TODO remove debug output
 * TODO reconnect
 */

/// Error type returned from this library's functions.
#[derive(Debug)]
pub enum Error {
    /// The HTTP request failed.
    HttpRequest(Box<std::error::Error + Send + 'static>),
    /// An error reading from the HTTP response body.
    HttpStream(Box<std::error::Error + Send + 'static>),
    /// The HTTP response stream ended unexpectedly (e.g. in the
    /// middle of an event).
    UnexpectedEof,
    /// Encountered a line not conforming to the SSE protocol.
    InvalidLine(String),
    /// Encountered an event type that is not a valid UTF-8 byte sequence.
    InvalidEventType(std::str::Utf8Error),
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
            Error::HttpRequest(err) => Some(err.as_ref()),
            Error::HttpStream(err) => Some(err.as_ref()),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
// TODO can we make this require less copying?
pub struct Event {
    pub event_type: String,
    fields: Map<String, Vec<u8>>,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Event {
    fn new() -> Event {
        Event {
            event_type: "".to_string(),
            fields: Map::new(),
        }
    }

    pub fn field(&self, name: &str) -> Option<&[u8]> {
        self.fields.get(name.into()).map(|buf| buf.as_slice())
    }

    fn set_field(&mut self, name: &str, value: &[u8]) {
        self.fields.insert(name.into(), value.to_owned());
    }
}

impl std::ops::Index<&str> for Event {
    type Output = [u8];

    fn index(&self, name: &str) -> &[u8] {
        &self.fields[name.into()]
    }
}

pub type EventStream = Box<Stream<Item = Event, Error = Error> + Send>;

pub struct ClientBuilder {
    url: r::Url,
    headers: r::header::HeaderMap,
}

impl ClientBuilder {
    /// Set a HTTP header on the SSE request.
    pub fn header(mut self, key: &'static str, value: &str) -> Result<ClientBuilder> {
        let value = value.parse().map_err(|e| Error::HttpRequest(Box::new(e)))?;
        self.headers.insert(key, value);
        Ok(self)
    }

    pub fn build(self) -> Client {
        Client {
            url: self.url,
            headers: self.headers,
        }
    }
}

/// Client that connects to a server using the Server-Sent Events protocol
/// and consumes the event stream indefinitely.
pub struct Client {
    url: r::Url,
    headers: r::header::HeaderMap,
}

impl Client {
    /// Construct a new `Client` (via a [`ClientBuilder`]). This will not
    /// perform any network activity until [`.stream()`] is called.
    ///
    /// [`ClientBuilder`]: struct.ClientBuilder.html
    /// [`.stream()`]: #method.stream
    pub fn for_url<U: r::IntoUrl>(url: U) -> Result<ClientBuilder> {
        let url = url
            .into_url()
            .map_err(|e| Error::HttpRequest(Box::new(e)))?;
        Ok(ClientBuilder {
            url: url,
            headers: r::header::HeaderMap::new(),
        })
    }

    /// Construct a new `Client` (via a [`ClientBuilder`]). This will not
    /// perform any network activity until [`.stream()`] is called.
    ///
    /// [`ClientBuilder`]: struct.ClientBuilder.html
    /// [`.stream()`]: #method.stream
    pub fn for_str(str_url: &str) -> Result<ClientBuilder> {
        let url = Url::parse(str_url).map_err(|e| Error::HttpRequest(Box::new(e)))?;
        Ok(ClientBuilder {
            url: url,
            headers: r::header::HeaderMap::new(),
        })
    }

    /// Connect to the server and begin consuming the stream. Produces a
    /// [`Stream`] of [`Event`]s.
    ///
    /// [`Stream`]: ../futures/stream/trait.Stream.html
    /// [`Event`]: struct.Event.html
    pub fn stream(&mut self) -> EventStream {
        let http = ra::Client::new();
        let request = http.get(self.url.clone()).headers(self.headers.clone());
        let resp = request.send();

        let fut_stream_chunks = resp
            .map_err(|e| Error::HttpRequest(Box::new(e)))
            .and_then(|resp| {
                debug!("HTTP response: {:#?}", resp);

                match resp.error_for_status() {
                    Ok(resp) => {
                        future::ok(resp.into_body().map_err(|e| Error::HttpStream(Box::new(e))))
                    }
                    Err(e) => future::err(Error::HttpRequest(Box::new(e))),
                }
            })
            .flatten_stream();

        Box::new(Decoded::new(fut_stream_chunks))
    }
}

const LOGIFY_MAX_CHARS: usize = 100;
fn logify(bytes: &[u8]) -> &str {
    let stringified = from_utf8(bytes).unwrap_or("<bad utf8>");
    if stringified.len() <= LOGIFY_MAX_CHARS {
        stringified
    } else {
        &stringified[..LOGIFY_MAX_CHARS - 1]
    }
}

fn parse_field(line: &[u8]) -> Result<Option<(&str, &[u8])>> {
    match line.iter().position(|&b| b':' == b) {
        Some(0) => {
            debug!(
                "comment: {}",
                from_utf8(&line[1..]).unwrap_or("<bad utf-8>")
            );
            Ok(None)
        }
        Some(colon_pos) => {
            let key = &line[0..colon_pos];
            let key = from_utf8(key)
                .map_err(|e| Error::InvalidLine(format!("malformed key: {:?}", e)))?;
            let value = &line[colon_pos + 1..];
            let value = match value.iter().position(|&b| !b.is_ascii_whitespace()) {
                Some(start) => &value[start..],
                None => b"",
            };

            debug!("key: {}, value: {}", key, logify(value));

            Ok(Some((key, value)))
        }
        None => Err(Error::InvalidLine("line missing ':' byte".to_string())),
    }
}

use futures::stream::Fuse;
use futures::{Async, Poll};

#[must_use = "streams do nothing unless polled"]
struct Decoded<S> {
    chunk_stream: Fuse<S>,
    incomplete_line: Option<Vec<u8>>,
    event: Option<Event>,
}

impl<S: Stream> Decoded<S> {
    fn new(s: S) -> Decoded<S> {
        return Decoded {
            chunk_stream: s.fuse(),
            incomplete_line: None,
            event: None,
        };
    }
}

impl<S> Stream for Decoded<S>
where
    S: Stream<Item = ra::Chunk>,
    Error: From<S::Error>,
{
    type Item = Event;
    type Error = Error;

    fn poll(&mut self) -> Poll<Option<Event>, Error> {
        trace!("Decoded::poll");

        loop {
            let chunk = match try_ready!(self.chunk_stream.poll()) {
                Some(c) => c,
                None => match self.incomplete_line {
                    None => return Ok(Async::Ready(None)),
                    Some(_) => return Err(Error::UnexpectedEof),
                },
            };

            trace!("decoder got a chunk: {:?}", logify(&chunk));

            match self.incomplete_line.as_mut() {
                // TODO can we avoid these copies?
                None => self.incomplete_line = Some(chunk.to_vec()),
                Some(incomplete_line) => incomplete_line.extend(chunk.into_iter()),
            }

            let incomplete_line = self.incomplete_line.as_ref().unwrap();
            let chunk = if incomplete_line.ends_with(b"\n") {
                std::mem::replace(&mut self.incomplete_line, None).unwrap()
            } else {
                debug!("Chunk does not end with newline!");
                continue;
            };
            // strip off final newline so that .split below doesn't yield a
            // bogus empty string as the last "line"
            let chunk = &chunk[..chunk.len() - 1];

            let lines = chunk.split(|&b| b'\n' == b);
            let mut seen_empty_line = false;

            for line in lines {
                trace!("Decoder got a line: {}", logify(line));

                if line.is_empty() {
                    trace!("empty line");
                    seen_empty_line = true;
                    continue;
                }

                if let Some((key, value)) = parse_field(line)? {
                    if self.event.is_none() {
                        self.event = Some(Event::new());
                    }

                    let mut event = self.event.as_mut().unwrap();

                    if key == "event" {
                        event.event_type = from_utf8(value)
                            .map_err(Error::InvalidEventType)?
                            .to_string();
                    } else {
                        event.set_field(key, value);
                    }
                }
            }

            trace!(
                "seen empty line: {} (event is {:?})",
                seen_empty_line,
                self.event.as_ref().map(|event| &event.event_type)
            );

            match (seen_empty_line, &self.event) {
                (_, None) => (),
                (true, Some(_)) => {
                    let event = std::mem::replace(&mut self.event, None);
                    return Ok(Async::Ready(event));
                }
                (false, Some(_)) => debug!("Haven't seen an empty line in this whole chunk, weird"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Error::*, *};

    fn invalid(msg: &str) -> Error {
        InvalidLine(msg.to_string())
    }

    fn field<'a>(key: &'a str, value: &'a [u8]) -> Result<Option<(&'a str, &'a [u8])>> {
        Ok(Some((key, value)))
    }

    #[test]
    fn test_parse_field_invalid() {
        assert_eq!(parse_field(b""), Err(invalid("line missing ':' byte")));

        assert_eq!(parse_field(b"event"), Err(invalid("line missing ':' byte")));

        match parse_field(b"\x80: invalid UTF-8") {
            Err(InvalidLine(msg)) => assert!(msg.contains("Utf8Error")),
            res => panic!("expected InvalidLine error, got {:?}", res),
        }
    }

    #[test]
    fn test_parse_field_comments() {
        assert_eq!(parse_field(b":"), Ok(None));
        assert_eq!(parse_field(b": hello \0 world"), Ok(None));
        assert_eq!(parse_field(b": event: foo"), Ok(None));
    }

    #[test]
    fn test_parse_field_valid() {
        assert_eq!(parse_field(b"event:foo"), field("event", b"foo"));
        assert_eq!(parse_field(b"event: foo"), field("event", b"foo"));
        assert_eq!(parse_field(b"event:  foo"), field("event", b"foo"));
        assert_eq!(parse_field(b"event:\tfoo"), field("event", b"foo"));
        assert_eq!(parse_field(b"event: foo "), field("event", b"foo "));

        assert_eq!(parse_field(b" : foo"), field(" ", b"foo"));
        assert_eq!(parse_field(b"\xe2\x98\x83: foo"), field("☃", b"foo"));
    }

    #[derive(Debug)]
    struct DummyError(&'static str);

    use std::fmt::{self, Display, Formatter};

    impl Display for DummyError {
        fn fmt(&self, f: &mut Formatter) -> std::result::Result<(), fmt::Error> {
            write!(f, "{}", self.0)
        }
    }

    impl std::error::Error for DummyError {}

    fn dummy_stream_error(msg: &'static str) -> Error {
        Error::HttpStream(Box::new(DummyError(msg)))
    }

    fn chunk(bytes: &[u8]) -> ra::Chunk {
        let mut chonk = ra::Chunk::default();
        chonk.extend(bytes.to_owned());
        chonk
    }

    fn event(typ: &str, fields: &Map<&str, &[u8]>) -> Event {
        let mut evt = Event::new();
        evt.event_type = typ.to_string();
        for (k, v) in fields {
            evt.set_field(k, v);
        }
        evt
    }

    use futures::stream;
    use Async::{NotReady, Ready};

    fn one_chunk(bytes: &[u8]) -> impl Stream<Item = ra::Chunk, Error = Error> {
        stream::once(Ok(chunk(bytes)))
    }

    fn delay_one_poll<T, E>() -> impl Stream<Item = T, Error = E> {
        let mut ready = false;

        stream::poll_fn(move || {
            if ready {
                Ok(Ready(None))
            } else {
                ready = true;
                Ok(NotReady)
            }
        })
    }

    fn delay_one_then<T, E>(t: T) -> impl Stream<Item = T, Error = E> {
        delay_one_poll().chain(stream::once(Ok(t)))
    }

    #[test]
    fn test_decod() {
        let empty = stream::empty::<ra::Chunk, Error>();
        assert_eq!(Decoded::new(empty).poll(), Ok(Ready(None)));

        assert_eq!(Decoded::new(one_chunk(b":hello\n")).poll(), Ok(Ready(None)));

        let one_comment_unterminated =
            futures::stream::once::<ra::Chunk, Error>(Ok(chunk(b":hello")));
        let mut decoded = Decoded::new(one_comment_unterminated);
        assert_eq!(decoded.poll(), Err(UnexpectedEof));

        assert_eq!(
            Decoded::new(one_chunk(b"message: hello\n")).poll(),
            Ok(Ready(None))
        );

        let interrupted_after_comment =
            one_chunk(b":hello\n").chain(stream::poll_fn(|| Err(dummy_stream_error("read error"))));
        match Decoded::new(interrupted_after_comment).poll() {
            Err(err) => {
                assert!(err.is_http_stream_error());
                let description = format!("{}", err.source().unwrap());
                assert!(description.contains("read error"), description);
            }
            res => panic!("expected HttpStream error, got {:?}", res),
        }

        let interrupted_after_field = one_chunk(b"message: hello\n")
            .chain(stream::poll_fn(|| Err(dummy_stream_error("read error"))));
        match Decoded::new(interrupted_after_field).poll() {
            Err(err) => {
                assert!(err.is_http_stream_error());
                let description = format!("{}", err.source().unwrap());
                assert!(description.contains("read error"), description);
            }
            res => panic!("expected HttpStream error, got {:?}", res),
        }

        let mut decoded = Decoded::new(one_chunk(b"message: hello\n\n"));
        assert_eq!(
            decoded.poll(),
            Ok(Ready(Some(event(
                "",
                &btreemap! {"message" => &b"hello"[..]}
            ))))
        );
        assert_eq!(decoded.poll(), Ok(Ready(None)));

        let mut decoded = Decoded::new(one_chunk(b"event: test\n\n"));
        assert_eq!(decoded.poll(), Ok(Ready(Some(event("test", &Map::new())))));
        assert_eq!(decoded.poll(), Ok(Ready(None)));

        let mut decoded = Decoded::new(one_chunk(b"event: test\nmessage: hello\nto: world\n\n"));
        assert_eq!(
            decoded.poll(),
            Ok(Ready(Some(event(
                "test",
                &btreemap! {"message" => &b"hello"[..], "to" => &b"world"[..]}
            ))))
        );
        assert_eq!(decoded.poll(), Ok(Ready(None)));

        let mut decoded = Decoded::new(one_chunk(b"message:").chain(one_chunk(b"hello\n\n")));
        assert_eq!(
            decoded.poll(),
            Ok(Ready(Some(event(
                "",
                &btreemap! {"message" => &b"hello"[..]}
            ))))
        );
        assert_eq!(decoded.poll(), Ok(Ready(None)));

        let mut decoded =
            Decoded::new(one_chunk(b"message:").chain(delay_one_then(chunk(b"hello\n\n"))));
        assert_eq!(decoded.poll(), Ok(NotReady));
        assert_eq!(
            decoded.poll(),
            Ok(Ready(Some(event(
                "",
                &btreemap! {"message" => &b"hello"[..]}
            ))))
        );
        assert_eq!(decoded.poll(), Ok(Ready(None)));

        let interrupted_after_event = one_chunk(b"message: hello\n\n")
            .chain(stream::poll_fn(|| Err(dummy_stream_error("read error"))));
        let mut decoded = Decoded::new(interrupted_after_event);
        assert_eq!(
            decoded.poll(),
            Ok(Ready(Some(event(
                "",
                &btreemap! {"message" => &b"hello"[..]}
            ))))
        );
        match decoded.poll() {
            Err(err) => {
                assert!(err.is_http_stream_error());
                let description = format!("{}", err.source().unwrap());
                assert!(description.contains("read error"), description);
            }
            res => panic!("expected HttpStream error, got {:?}", res),
        }

        let mut decoded = Decoded::new(one_chunk(b"event: \x80\n\n"));
        match decoded.poll() {
            Err(InvalidEventType(_)) => (),
            res => panic!("expected InvalidEventType error, got {:?}", res),
        }

        let mut decoded = Decoded::new(one_chunk(b"\x80: invalid UTF-8\n\n"));
        match decoded.poll() {
            Err(InvalidLine(_)) => (),
            res => panic!("expected InvalidLine error, got {:?}", res),
        }
    }
}
