use std::collections::BTreeMap as Map;
use std::collections::VecDeque;
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
    if line.is_empty() {
        return Err(Error::InvalidLine(
            "should never try to parse an empty line (probably a bug)".into(),
        ));
    }

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
            let key = parse_key(key)?;

            let mut value = &line[colon_pos + 1..];
            // remove the first initial space character if any (but remove no other whitespace)
            if value.starts_with(b" ") {
                value = &value[1..];
            }

            debug!("key: {}, value: {}", key, logify(value));

            Ok(Some((key, value)))
        }
        None => Ok(Some((parse_key(line)?, b""))),
    }
}

fn parse_key(key: &[u8]) -> Result<&str> {
    from_utf8(key).map_err(|e| Error::InvalidLine(format!("malformed key: {:?}", e)))
}

#[must_use = "streams do nothing unless polled"]
pub struct Decoded<S> {
    /// the underlying byte stream to decode
    chunk_stream: Fuse<S>,
    /// buffer for lines we know are complete (terminated) but not yet parsed into event fields, in
    /// the order received
    complete_lines: VecDeque<Vec<u8>>,
    /// buffer for the most-recently received line, pending completion (by a newline terminator) or
    /// extension (by more non-newline bytes)
    incomplete_line: Option<Vec<u8>>,
    /// the event currently being decoded
    /// TODO does this need to be Option?
    event: Option<Event>,
}

impl<S: Stream> Decoded<S> {
    pub fn new(s: S) -> Decoded<S> {
        Decoded {
            chunk_stream: s.fuse(),
            complete_lines: VecDeque::with_capacity(10),
            incomplete_line: None,
            event: None,
        }
    }

    // Populate the event fields from the complete lines already seen, until we either encounter an
    // empty line - indicating we've decoded a complete event - or we run out of complete lines to
    // process.
    //
    // Returns the event for dispatch if it is complete.
    fn parse_complete_lines_into_event(&mut self) -> Result<Option<Event>> {
        let mut seen_empty_line = false;

        while let Some(line) = self.complete_lines.pop_front() {
            if line.is_empty() {
                seen_empty_line = true;
                break;
            }

            if let Some((key, value)) = parse_field(&line)? {
                let event = self.event.get_or_insert_with(Event::new);

                if key == "event" {
                    event.event_type = from_utf8(value)
                        .map_err(Error::InvalidEventType)?
                        .to_string();
                } else {
                    event.append_field(key, value);
                }
            }
        }

        if seen_empty_line {
            let event = self.event.take();
            trace!(
                "seen empty line, event is {:?})",
                event.as_ref().map(|event| &event.event_type)
            );
            Ok(event)
        } else {
            trace!("processed all complete lines but event not yet complete");
            Ok(None)
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
            // TODO this comment no longer makes any sense
            // TODO decompose this loop into functions

            // Phase 2: decode the lines into events.

            if let Some(event) = self.parse_complete_lines_into_event()? {
                debug!("decoded a complete event");
                return Ok(Async::Ready(Some(event)));
            }

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

            // TODO(ch86257) also handle lines ending in \r, \r\n (and EOF?)
            let lines = chunk.split(|&b| b == b'\n');
            // The first and last elements in this split are special. The spec requires lines to be
            // terminated. But lines may span chunks, so:
            //  * the last line, if non-empty (i.e. if chunk didn't end with a line terminator),
            //    should be buffered as an incomplete line
            //  * the first line should be appended to the incomplete line, if any

            let mut maybe_incomplete_line: Option<Vec<u8>> = None;

            for line in lines {
                if let Some(incomplete_line) = &mut self.incomplete_line {
                    // only the first line can hit this case, since it clears self.incomplete_line
                    // and we don't fill it again until the end of the loop

                    trace!(
                        "completing line from previous chunk: {:?}+{:?}",
                        logify(&incomplete_line),
                        logify(line)
                    );

                    let mut incomplete_line =
                        std::mem::replace(&mut self.incomplete_line, None).unwrap();
                    incomplete_line.extend_from_slice(line);

                    maybe_incomplete_line = Some(incomplete_line); // safe to clobber since this is the first line
                    continue;
                }

                match &mut maybe_incomplete_line {
                    None => {
                        trace!("potentially incomplete line: {:?}", logify(line));
                        maybe_incomplete_line = Some(line.to_vec());
                    }
                    Some(actually_complete_line) => {
                        // we saw the next line, so the previous one must have been complete after all
                        trace!(
                            "previous line was complete: {:?}",
                            logify(actually_complete_line)
                        );
                        let actually_complete_line =
                            std::mem::replace(actually_complete_line, line.to_vec());
                        self.complete_lines.push_back(actually_complete_line);
                    }
                }
            }

            match maybe_incomplete_line {
                Some(l) if l.is_empty() => trace!("chunk ended with a line terminator"),
                Some(incomplete_line) => {
                    trace!("buffering incomplete line: {:?}", logify(&incomplete_line));
                    self.incomplete_line = Some(incomplete_line);
                }
                None => unreachable!(), // we always set it after processing a line, and we always have at least one line
            }

            if log_enabled!(log::Level::Trace) {
                for line in &self.complete_lines {
                    trace!("complete line: {:?}", logify(line));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Error::*, *};

    fn field<'a>(key: &'a str, value: &'a [u8]) -> Result<Option<(&'a str, &'a [u8])>> {
        Ok(Some((key, value)))
    }

    #[test]
    fn test_parse_field_invalid() {
        assert!(parse_field(b"").is_err());

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
        assert_eq!(parse_field(b"event:  foo"), field("event", b" foo"));
        assert_eq!(parse_field(b"event:\tfoo"), field("event", b"\tfoo"));
        assert_eq!(parse_field(b"event: foo "), field("event", b"foo "));

        assert_eq!(parse_field(b"disconnect:"), field("disconnect", b""));
        assert_eq!(parse_field(b"disconnect: "), field("disconnect", b""));
        assert_eq!(parse_field(b"disconnect:  "), field("disconnect", b" "));
        assert_eq!(parse_field(b"disconnect:\t"), field("disconnect", b"\t"));

        assert_eq!(parse_field(b"disconnect"), field("disconnect", b""));

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
    fn test_decode_chunks_simple() {
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

        assert_eq!(
            Decoded::new(one_chunk(b":hello\n")).poll(),
            Ok(Ready(None)),
            "comments are ignored"
        );
    }

    #[test]
    fn test_decode_message_split_across_chunks() {
        let mut decoded = Decoded::new(one_chunk(b"message:").chain(one_chunk(b"hello\n\n")));
        assert_eq!(
            decoded.poll(),
            Ok(Ready(Some(event(
                "",
                &btreemap! {"message" => &b"hello"[..]}
            ))))
        );
        assert_eq!(decoded.poll(), Ok(Ready(None)));

        let mut decoded = Decoded::new(one_chunk(b"message:hell").chain(one_chunk(b"o\n\n")));
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

        let mut decoded = Decoded::new(
            one_chunk(b"message:hell")
                .chain(one_chunk(b"o\n\nmessage:"))
                .chain(one_chunk(b"world\n\n")),
        );

        assert_eq!(
            decoded.poll(),
            Ok(Ready(Some(event(
                "",
                &btreemap! {"message" => &b"hello"[..]}
            ))))
        );
        assert_eq!(
            decoded.poll(),
            Ok(Ready(Some(event(
                "",
                &btreemap! {"message" => &b"world"[..]}
            ))))
        );
        assert_eq!(decoded.poll(), Ok(Ready(None)));
    }

    #[test]
    fn test_decode_line_split_across_chunks() {
        let empty_after_incomplete = one_chunk(b"message:foo")
            .chain(one_chunk(b""))
            .chain(one_chunk(b"baz\n\n"));
        let mut decoded = Decoded::new(empty_after_incomplete);
        assert_eq!(
            decoded.poll(),
            Ok(Ready(Some(event(
                "",
                &btreemap! {"message" => &b"foobaz"[..]}
            ))))
        );

        let incomplete_after_incomplete = one_chunk(b"message:foo")
            .chain(one_chunk(b"bar"))
            .chain(one_chunk(b"baz\n\n"));
        let mut decoded = Decoded::new(incomplete_after_incomplete);
        assert_eq!(
            decoded.poll(),
            Ok(Ready(Some(event(
                "",
                &btreemap! {"message" => &b"foobarbaz"[..]}
            ))))
        );
    }

    #[test]
    fn test_decode_concatenates_multiple_values_for_same_field() {
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
    }

    #[test]
    fn test_decode_edge_cases() {
        let empty = stream::empty::<ra::Chunk, Error>();
        assert_eq!(Decoded::new(empty).poll(), Ok(Ready(None)));

        let one_empty = one_chunk(b"");
        assert_eq!(Decoded::new(one_empty).poll(), Ok(Ready(None)));

        let one_comment_unterminated = one_chunk(b":hello");
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

    #[test]
    fn test_decode_one_event() {
        let stream = one_chunk_from_file("one-event.sse");
        let mut decoded = Decoded::new(stream);

        let event = next_event(&mut decoded);
        assert_eq!(event.event_type, "patch");
        let data = event_data(&event).expect("event data should parse");
        assert!(data.contains(r#"path":"/flags/goals.02.featureWithGoals"#));

        assert_eof(decoded);
    }

    #[test]
    fn test_decode_two_events() {
        let stream = one_chunk_from_file("two-events.sse");
        let mut decoded = Decoded::new(stream);

        let event = next_event(&mut decoded);
        assert_eq!(event.event_type, "one");
        let data = event_data(&event).expect("event data should parse");
        assert_eq!(data, "One\n");

        let event = next_event(&mut decoded);
        assert_eq!(event.event_type, "two");
        let data = event_data(&event).expect("event data should parse");
        assert_eq!(data, "Two\n");

        assert_eof(decoded);
    }

    #[test]
    fn test_decode_big_event_followed_by_another() {
        let stream = one_chunk_from_file("big-event-followed-by-another.sse");
        let mut decoded = Decoded::new(stream);

        let event = next_event(&mut decoded);
        assert_eq!(event.event_type, "patch");
        let data = event_data(&event).expect("event data should parse");
        assert!(data.len() > 10_000);
        assert!(data.contains(r#"path":"/flags/big.00.bigFeatureKey"#));

        let event = next_event(&mut decoded);
        assert_eq!(event.event_type, "patch");
        let data = event_data(&event).expect("event data should parse");
        assert!(data.contains(r#"path":"/flags/goals.02.featureWithGoals"#));

        assert_eof(decoded);
    }

    fn one_chunk_from_file(name: &str) -> impl Stream<Item = ra::Chunk, Error = Error> {
        let bytes =
            std::fs::read(format!("test-data/{}", name)).expect(&format!("couldn't read {}", name));
        one_chunk(&bytes)
    }

    fn event_data(event: &Event) -> std::result::Result<&str, String> {
        let data = event.field("data").ok_or("no data field")?;
        from_utf8(data).map_err(|e| format!("invalid UTF-8: {}", e))
    }

    fn assert_eof<S>(mut s: Decoded<S>)
    where
        S: Stream<Item = ra::Chunk>,
        Error: From<S::Error>,
    {
        match s.poll().expect("poll should succeed") {
            Ready(None) => (),
            Ready(Some(event)) => panic!(format!("expected eof, got {:?}", event)),
            NotReady => panic!("expected eof, got NotReady"),
        }
    }

    fn next_event<S>(s: &mut Decoded<S>) -> Event
    where
        S: Stream<Item = ra::Chunk>,
        Error: From<S::Error>,
    {
        match s.poll().expect("poll should succeed") {
            Ready(Some(event)) => event,
            Ready(None) => panic!("expected event, got eof"),
            NotReady => panic!("expected event, got NotReady"),
        }
    }
}
