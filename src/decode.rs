use std::collections::BTreeMap as Map;
use std::str::from_utf8;

use futures::stream::{Fuse, Stream};
use futures::{Async, Poll};
use reqwest::r#async as ra;

use super::error::{Error, Result};

pub type EventStream = Box<dyn Stream<Item = Event, Error = Error> + Send>;

#[derive(Clone, Debug, PartialEq)]
// TODO can we make this require less copying?
pub struct Event {
    pub event_type: String,
    fields: Map<String, Vec<u8>>,
}

impl Event {
    fn new() -> Event {
        Event {
            event_type: "".to_string(),
            fields: Map::new(),
        }
    }

    pub fn field(&self, name: &str) -> Option<&[u8]> {
        self.fields.get(name).map(|buf| buf.as_slice())
    }

    // Set the named field to the given value, or append to any existing value, as required by the
    // spec. Will append a newline each time.
    fn append_field(&mut self, name: &str, value: &[u8]) {
        let existing = match self.fields.get_mut(name) {
            None => {
                let empty = Vec::with_capacity(value.len() + 1);
                self.fields.insert(name.into(), empty);
                self.fields.get_mut(name).unwrap()
            }
            Some(nonempty) => {
                nonempty.reserve(value.len() + 1);
                nonempty
            }
        };

        existing.extend(value);
        existing.push(b'\n');
    }
}

impl std::ops::Index<&str> for Event {
    type Output = [u8];

    fn index(&self, name: &str) -> &[u8] {
        &self.fields[name]
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

#[must_use = "streams do nothing unless polled"]
pub struct Decoded<S> {
    chunk_stream: Fuse<S>,
    incomplete_line: Option<Vec<u8>>,
    event: Option<Event>,
}

impl<S: Stream> Decoded<S> {
    pub fn new(s: S) -> Decoded<S> {
        Decoded {
            chunk_stream: s.fuse(),
            incomplete_line: None,
            event: None,
        }
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

            // Decoding a chunk has two phases: decode the chunk into lines, and decode the lines
            // into events.

            // Phase 1: decode the chunk into lines.

            let mut complete_lines: Vec<Vec<u8>> = Vec::with_capacity(10);
            let mut maybe_incomplete_line: Option<&[u8]> = None;

            // TODO also handle lines ending in \r, \r\n (and EOF?)
            let mut lines = chunk.split(|&b| b == b'\n');
            // The first and last elements in this split are special. The spec requires lines to be
            // terminated. But lines may span chunks, so:
            //  * the last line, if non-empty (i.e. if chunk didn't end with a line terminator),
            //    should be buffered as an incomplete line
            //  * the first line should be appended to the incomplete line, if any

            for line in lines {
                trace!("decoder got a line: {:?}", logify(line));

                if self.incomplete_line.is_some() {
                    trace!(
                        "completing line: {:?}",
                        logify(self.incomplete_line.as_ref().unwrap())
                    );

                    // only the first line can hit this case, since it clears self.incomplete_line
                    // and we don't fill it again until the end of the loop
                    let mut incomplete_line =
                        std::mem::replace(&mut self.incomplete_line, None).unwrap();
                    incomplete_line.extend_from_slice(line);
                    complete_lines.push(incomplete_line);
                    continue;
                }

                if maybe_incomplete_line.is_some() {
                    // we saw the next line, so the previous one must have been complete after all
                    trace!(
                        "previous line was complete: {:?}",
                        logify(maybe_incomplete_line.as_ref().unwrap())
                    );
                    let actually_complete_line =
                        std::mem::replace(&mut maybe_incomplete_line, Some(line)).unwrap();
                    complete_lines.push(actually_complete_line.to_vec());
                } else {
                    trace!("potentially incomplete line: {:?}", logify(line));
                    maybe_incomplete_line = Some(line);
                }
            }

            match maybe_incomplete_line {
                Some(b"") => trace!("last line was empty"),
                Some(incomplete_line) => {
                    trace!("buffering incomplete line: {:?}", logify(incomplete_line));
                    self.incomplete_line = Some(incomplete_line.to_vec());
                }
                None => trace!("no last line?"), // TODO
            }

            for line in &complete_lines {
                trace!("complete line: {:?}", logify(line));
            }

            // Phase 2: decode the lines into events.

            let mut seen_empty_line = false;

            for line in complete_lines {
                trace!("Decoder got a line: {:?}", logify(&line));

                if line.is_empty() {
                    trace!("empty line");
                    seen_empty_line = true;
                    continue;
                }

                if let Some((key, value)) = parse_field(&line)? {
                    if self.event.is_none() {
                        self.event = Some(Event::new());
                    }

                    let mut event = self.event.as_mut().unwrap();

                    if key == "event" {
                        event.event_type = from_utf8(value)
                            .map_err(Error::InvalidEventType)?
                            .to_string();
                    } else {
                        event.append_field(key, value);
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
            evt.append_field(k, v);
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

        let mut decoded =
            Decoded::new(one_chunk(b"message:hell").chain(delay_one_then(chunk(b"o\n\n"))));
        assert_eq!(decoded.poll(), Ok(NotReady));
        assert_eq!(
            decoded.poll(),
            Ok(Ready(Some(event(
                "",
                &btreemap! {"message" => &b"hello"[..]}
            ))))
        );
        assert_eq!(decoded.poll(), Ok(Ready(None)));

        let mut decoded = Decoded::new(
            one_chunk(b"message:hell")
                .chain(delay_one_then(chunk(b"o\n\nmessage:")))
                .chain(delay_one_then(chunk(b"world\n\n"))),
        );

        assert_eq!(decoded.poll(), Ok(NotReady));
        assert_eq!(
            decoded.poll(),
            Ok(Ready(Some(event(
                "",
                &btreemap! {"message" => &b"hello"[..]}
            ))))
        );
        assert_eq!(decoded.poll(), Ok(NotReady));
        assert_eq!(
            decoded.poll(),
            Ok(Ready(Some(event(
                "",
                &btreemap! {"message" => &b"world"[..]}
            ))))
        );
        assert_eq!(decoded.poll(), Ok(Ready(None)));

        let mut decoded = Decoded::new(
            one_chunk(b"data:hello\n").chain(delay_one_then(chunk(b"data:world\n\n"))),
        );

        assert_eq!(decoded.poll(), Ok(NotReady));
        assert_eq!(
            decoded.poll(),
            Ok(Ready(Some(event(
                "",
                &btreemap! {"data" => &b"hello\nworld"[..]}
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
