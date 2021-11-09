use std::{
    collections::{BTreeMap as Map, VecDeque},
    pin::Pin,
    str::from_utf8,
    task::{Context, Poll},
};

use futures::{
    ready,
    stream::{Fuse, Stream},
    StreamExt,
};
use hyper::body::Bytes;
use log::{debug, log_enabled, trace};
use pin_project::pin_project;

use super::error::{Error, Result};

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

#[pin_project]
#[must_use = "streams do nothing unless polled"]
pub struct Decoded<S> {
    /// the underlying byte stream to decode
    #[pin]
    chunk_stream: Fuse<S>,
    /// buffer for lines we know are complete (terminated) but not yet parsed into event fields, in
    /// the order received
    complete_lines: VecDeque<Vec<u8>>,
    /// buffer for the most-recently received line, pending completion (by a newline terminator) or
    /// extension (by more non-newline bytes)
    incomplete_line: Option<Vec<u8>>,
    /// flagged if the last character processed as a carriage return; used to help process CRLF
    /// pairs
    last_char_was_cr: bool,
    /// the event currently being decoded
    event: Option<Event>,
}

impl<S: Stream> Decoded<S> {
    pub fn new(s: S) -> Decoded<S> {
        Decoded {
            chunk_stream: s.fuse(),
            complete_lines: VecDeque::with_capacity(10),
            incomplete_line: None,
            last_char_was_cr: false,
            event: None,
        }
    }

    // Populate the event fields from the complete lines already seen, until we either encounter an
    // empty line - indicating we've decoded a complete event - or we run out of complete lines to
    // process.
    //
    // Returns the event for dispatch if it is complete.
    fn parse_complete_lines_into_event(mut self: Pin<&mut Self>) -> Result<Option<Event>> {
        trace!("***** we are going to start the parsing process");
        let mut seen_empty_line = false;

        let this = self.as_mut().project();

        while let Some(line) = this.complete_lines.pop_front() {
            trace!("***** initial line received is {:?})", line);
            if line.is_empty() {
                seen_empty_line = true;
                break;
            }

            if let Some((key, value)) = parse_field(&line)? {
                let event = this.event.get_or_insert_with(Event::new);

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
            let event = this.event.take();
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

    // Decode a chunk into lines and buffer them for subsequent parsing, taking account of
    // incomplete lines from previous chunks.
    fn decode_and_buffer_lines(mut self: Pin<&mut Self>, chunk: Bytes) {
        let this = self.as_mut().project();

        let mut lines = chunk.split_inclusive(|&b| b == b'\n' || b == b'\r');

        // The first and last elements in this split are special. The spec requires lines to be
        // terminated. But lines may span chunks, so:
        //  * the last line, if non-empty (i.e. if chunk didn't end with a line terminator),
        //    should be buffered as an incomplete line
        //  * the first line should be appended to the incomplete line, if any

        if let Some(incomplete_line) = this.incomplete_line.as_mut() {
            let line = lines
                .next()
                // split always returns at least one item
                .unwrap();
            trace!(
                "extending line from previous chunk: {:?}+{:?}",
                logify(incomplete_line),
                logify(line)
            );

            *this.last_char_was_cr = false;
            if !line.is_empty() {
                // Checking the last character handles lines where the last character is a
                // terminator, but also where the entire line is a terminator.
                match line.last().unwrap() {
                    b'\r' => {
                        incomplete_line.extend_from_slice(&line[..line.len() - 1]);
                        *this.last_char_was_cr = true;
                    }
                    b'\n' => incomplete_line.extend_from_slice(&line[..line.len() - 1]),
                    _ => incomplete_line.extend_from_slice(line),
                };
            }
        }

        let mut lines = lines.peekable();
        while let Some(line) = lines.next() {
            if let Some(actually_complete_line) = this.incomplete_line.take() {
                // we saw the next line, so the previous one must have been complete after all
                trace!(
                    "previous line was complete: {:?}",
                    logify(&actually_complete_line)
                );
                this.complete_lines.push_back(actually_complete_line);
            }

            if *this.last_char_was_cr && line == [b'\n'] {
                // This is a continuation of a \r\n pair, so we can ignore this line. We do need to
                // reset our flag though.
                *this.last_char_was_cr = false;
                continue;
            }

            *this.last_char_was_cr = false;
            if line.ends_with(&[b'\r']) {
                this.complete_lines
                    .push_back(line[..line.len() - 1].to_vec());
                *this.last_char_was_cr = true;
            } else if line.ends_with(&[b'\n']) {
                // This isn't a continuation, but rather a line ending with a LF terminator.
                this.complete_lines
                    .push_back(line[..line.len() - 1].to_vec());
            } else if line.is_empty() {
                // this is the last line and it's empty, no need to buffer it
                trace!("chunk ended with a line terminator");
            } else if lines.peek().is_some() {
                // this line isn't the last and we know from previous checks it doesn't end in a
                // terminator, so we can consider it complete
                this.complete_lines.push_back(line.to_vec());
            } else {
                // last line needs to be buffered as it may be incomplete
                trace!("buffering incomplete line: {:?}", logify(line));
                *this.incomplete_line = Some(line.to_vec());
            }
        }

        if log_enabled!(log::Level::Trace) {
            for line in &self.complete_lines {
                trace!("complete line: {:?}", logify(line));
            }
            if let Some(line) = &self.incomplete_line {
                trace!("incomplete line: {:?}", logify(line));
            }
        }
    }
}

impl<S> Stream for Decoded<S>
where
    S: Stream<Item = Result<Bytes>>,
{
    type Item = Result<Event>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        trace!("Decoded::poll");

        // We get bytes from the underlying stream in chunks.  Decoding a chunk has two phases:
        // decode the chunk into lines, and decode the lines into events.
        //
        // We counterintuitively do these two phases in reverse order. Because both lines and
        // events may be split across chunks, we need to ensure we have a complete
        // (newline-terminated) line before parsing it, and a complete event
        // (empty-line-terminated) before returning it. So we buffer lines between poll()
        // invocations, and begin by processing any incomplete events from previous invocations,
        // before requesting new input from the underlying stream and processing that.

        loop {
            if let Some(event) = self.as_mut().parse_complete_lines_into_event()? {
                debug!("decoded a complete event");
                return Poll::Ready(Some(Ok(event)));
            }

            let this = self.as_mut().project();

            let chunk = match ready!(this.chunk_stream.poll_next(cx)) {
                Some(Ok(c)) => {
                    trace!("decoder got a chunk: {:?}", logify(&c));
                    c
                }
                Some(Err(e)) => {
                    trace!("{:?}", e);
                    return Poll::Ready(Some(Err(e)));
                }
                None => {
                    return match this.incomplete_line {
                        None => {
                            debug!("end of stream: no more chunks and nothing in the buffer");
                            Poll::Ready(None)
                        }
                        Some(_) => {
                            debug!("unexpected end of stream: no more chunks but we still have an unterminated line buffered");
                            Poll::Ready(Some(Err(Error::UnexpectedEof)))
                        }
                    };
                }
            };

            self.as_mut().decode_and_buffer_lines(chunk);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Error::*, *};
    use test_case::test_case;

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

    use std::{
        fmt::{self, Display, Formatter},
        iter,
    };

    impl Display for DummyError {
        fn fmt(&self, f: &mut Formatter) -> std::result::Result<(), fmt::Error> {
            write!(f, "{}", self.0)
        }
    }

    impl std::error::Error for DummyError {}

    fn dummy_stream_error(msg: &'static str) -> Error {
        Error::HttpStream(Box::new(DummyError(msg)))
    }

    fn chunk(bytes: &[u8]) -> Bytes {
        Bytes::copy_from_slice(bytes)
    }

    fn event(typ: &str, fields: &Map<&str, &[u8]>) -> Event {
        let mut evt = Event::new();
        evt.event_type = typ.to_string();
        for (k, v) in fields {
            evt.append_field(k, v);
        }
        evt
    }

    use futures::{stream, task::noop_waker, Stream};
    use maplit::btreemap;

    fn one_chunk(bytes: &[u8]) -> impl Stream<Item = Result<Bytes>> + Unpin {
        let chunk = chunk(bytes);
        stream::iter(iter::once(Ok(chunk)))
    }

    fn delay_one_poll<T, E>() -> impl Stream<Item = core::result::Result<T, E>> + Unpin {
        let mut ready = false;
        stream::poll_fn(move |_cx| {
            if !ready {
                ready = true;
                return Poll::Pending;
            }
            Poll::Ready(None)
        })
        .fuse()
    }

    fn delay_one_then<T, E>(t: T) -> impl Stream<Item = core::result::Result<T, E>> + Unpin {
        delay_one_poll().chain(stream::iter(iter::once(Ok(t))))
    }

    #[test_case(b"message: hello\n\n", event("", &btreemap! {"message" => &b"hello"[..]}); "parses event body with LF")]
    #[test_case(b"message: hello\n\r", event("", &btreemap! {"message" => &b"hello"[..]}); "parses event body with LF and trailing CR")]
    #[test_case(b"message: hello\r\n\n", event("", &btreemap! {"message" => &b"hello"[..]}); "parses event body with CRLF")]
    #[test_case(b"message: hello\r\n\r", event("", &btreemap! {"message" => &b"hello"[..]}); "parses event body with CRLF and trailing CR")]
    #[test_case(b"message: hello\r\r", event("", &btreemap! {"message" => &b"hello"[..]}); "parses event body with CR")]
    #[test_case(b"message: hello\r\r\n", event("", &btreemap! {"message" => &b"hello"[..]}); "parses event body with CR and trailing CRLF")]
    #[test_case(b"event: test\n\n", event("test", &Map::new()); "determines event name with trailing LF")]
    #[test_case(b"event: test\nmessage: hello\nto: world\n\n", event( "test", &btreemap! {"message" => &b"hello"[..], "to" => &b"world"[..]}); "parses full event with LF")]
    #[test_case(b"event: test\rmessage: hello\rto: world\r\r", event( "test", &btreemap! {"message" => &b"hello"[..], "to" => &b"world"[..]}); "parses full event with CR")]
    #[test_case(b"event: test\r\nmessage: hello\r\nto: world\r\n\n", event( "test", &btreemap! {"message" => &b"hello"[..], "to" => &b"world"[..]}); "parses full event with CRLF")]
    fn test_decode_chunks_simple(chunk: &[u8], event: Event) {
        use Poll::Ready;
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);

        let mut decoded = Decoded::new(one_chunk(chunk));
        assert_eq!(decoded.poll_next_unpin(&mut cx), Ready(Some(Ok(event))));
        assert_eq!(decoded.poll_next_unpin(&mut cx), Ready(None));
    }

    #[test_case(b":hello\n"; "with LF")]
    #[test_case(b":hello\r"; "with CR")]
    #[test_case(b":hello\r\n"; "with CRLF")]
    fn test_decode_chunks_comments_are_ignored(chunk: &[u8]) {
        use Poll::Ready;
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);

        let mut decoded = Decoded::new(one_chunk(chunk));
        assert_eq!(decoded.poll_next_unpin(&mut cx), Ready(None));
    }

    #[test]
    fn test_decode_message_split_across_chunks() {
        use Poll::{Pending, Ready};
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);

        let mut decoded = Decoded::new(one_chunk(b"message:").chain(one_chunk(b"hello\n\n")));
        assert_eq!(
            decoded.poll_next_unpin(&mut cx),
            Ready(Some(Ok(event("", &btreemap! {"message" => &b"hello"[..]}))))
        );
        assert_eq!(decoded.poll_next_unpin(&mut cx), Ready(None));

        let mut decoded = Decoded::new(one_chunk(b"message:hell").chain(one_chunk(b"o\n\n")));
        assert_eq!(
            decoded.poll_next_unpin(&mut cx),
            Ready(Some(Ok(event("", &btreemap! {"message" => &b"hello"[..]}))))
        );
        assert_eq!(decoded.poll_next_unpin(&mut cx), Ready(None));

        let mut decoded =
            Decoded::new(one_chunk(b"message:").chain(delay_one_then(chunk(b"hello\n\n"))));
        assert_eq!(decoded.poll_next_unpin(&mut cx), Pending);
        assert_eq!(
            decoded.poll_next_unpin(&mut cx),
            Ready(Some(Ok(event("", &btreemap! {"message" => &b"hello"[..]}))))
        );
        assert_eq!(decoded.poll_next_unpin(&mut cx), Ready(None));

        let mut decoded = Decoded::new(
            one_chunk(b"message:hell")
                .chain(one_chunk(b"o\n\nmessage:"))
                .chain(one_chunk(b"world\n\n")),
        );
        assert_eq!(
            decoded.poll_next_unpin(&mut cx),
            Ready(Some(Ok(event("", &btreemap! {"message" => &b"hello"[..]}))))
        );
        assert_eq!(
            decoded.poll_next_unpin(&mut cx),
            Ready(Some(Ok(event("", &btreemap! {"message" => &b"world"[..]}))))
        );
        assert_eq!(decoded.poll_next_unpin(&mut cx), Ready(None));

        let mut decoded = Decoded::new(
            one_chunk(b"message:hell")
                .chain(one_chunk(b"o\r\rmessage:"))
                .chain(one_chunk(b"world\r\r")),
        );
        assert_eq!(
            decoded.poll_next_unpin(&mut cx),
            Ready(Some(Ok(event("", &btreemap! {"message" => &b"hello"[..]}))))
        );
        assert_eq!(
            decoded.poll_next_unpin(&mut cx),
            Ready(Some(Ok(event("", &btreemap! {"message" => &b"world"[..]}))))
        );
        assert_eq!(decoded.poll_next_unpin(&mut cx), Ready(None));

        let mut decoded = Decoded::new(
            one_chunk(b"message:hell")
                .chain(one_chunk(b"o\r\n\nmessage:"))
                .chain(one_chunk(b"world\r\n\n")),
        );
        assert_eq!(
            decoded.poll_next_unpin(&mut cx),
            Ready(Some(Ok(event("", &btreemap! {"message" => &b"hello"[..]}))))
        );
        assert_eq!(
            decoded.poll_next_unpin(&mut cx),
            Ready(Some(Ok(event("", &btreemap! {"message" => &b"world"[..]}))))
        );
        assert_eq!(decoded.poll_next_unpin(&mut cx), Ready(None));
    }

    #[test]
    fn test_decode_line_split_across_chunks() {
        use Poll::Ready;
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);

        let empty_after_incomplete = one_chunk(b"message:foo")
            .chain(one_chunk(b""))
            .chain(one_chunk(b"baz\n\n"));
        let mut decoded = Decoded::new(empty_after_incomplete);
        assert_eq!(
            decoded.poll_next_unpin(&mut cx),
            Ready(Some(Ok(event(
                "",
                &btreemap! {"message" => &b"foobaz"[..]}
            ))))
        );

        let incomplete_after_incomplete = one_chunk(b"message:foo")
            .chain(one_chunk(b"bar"))
            .chain(one_chunk(b"baz\n\n"));
        let mut decoded = Decoded::new(incomplete_after_incomplete);
        assert_eq!(
            decoded.poll_next_unpin(&mut cx),
            Ready(Some(Ok(event(
                "",
                &btreemap! {"message" => &b"foobarbaz"[..]}
            ))))
        );
    }

    #[test]
    fn test_decode_concatenates_multiple_values_for_same_field() {
        use Poll::{Pending, Ready};
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);

        let mut decoded = Decoded::new(
            one_chunk(b"data:hello\n").chain(delay_one_then(chunk(b"data:world\n\n"))),
        );

        assert_eq!(decoded.poll_next_unpin(&mut cx), Pending);
        assert_eq!(
            decoded.poll_next_unpin(&mut cx),
            Ready(Some(Ok(event(
                "",
                &btreemap! {"data" => &b"hello\nworld"[..]}
            ))))
        );
        assert_eq!(decoded.poll_next_unpin(&mut cx), Ready(None));
    }

    #[test_case(b"\n\n\n\n" ; "all LFs")]
    #[test_case(b"\r\r\r\r" ; "all CRs")]
    #[test_case(b"\r\n\r\n\r\n\r\n" ; "all CRLFs")]
    fn test_decode_repeated_terminators(bytes: &[u8]) {
        use Poll::Ready;
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);

        // spec seems unclear on whether this should actually dispatch empty events, but that seems
        // unhelpful for all practical purposes
        let repeated_newlines = one_chunk(bytes);
        assert_eq!(
            Decoded::new(repeated_newlines).poll_next_unpin(&mut cx),
            Ready(None)
        );
    }

    #[test]
    fn test_decode_edge_cases() {
        use Poll::Ready;
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);

        let empty = stream::empty::<Result<Bytes>>();
        assert_eq!(Decoded::new(empty).poll_next_unpin(&mut cx), Ready(None));

        let one_empty = one_chunk(b"");
        assert_eq!(
            Decoded::new(one_empty).poll_next_unpin(&mut cx),
            Ready(None)
        );

        let one_comment_unterminated = one_chunk(b":hello");
        let mut decoded = Decoded::new(one_comment_unterminated);
        assert_eq!(
            decoded.poll_next_unpin(&mut cx),
            Ready(Some(Err(UnexpectedEof)))
        );

        assert_eq!(
            Decoded::new(one_chunk(b"message: hello\n")).poll_next_unpin(&mut cx),
            Ready(None)
        );

        let interrupted_after_comment = one_chunk(b":hello\n").chain(stream::poll_fn(|_cx| {
            Ready(Some(Err(dummy_stream_error("read error"))))
        }));
        match Decoded::new(interrupted_after_comment).poll_next_unpin(&mut cx) {
            Ready(Some(Err(err))) => {
                assert!(err.is_http_stream_error());
                let description = format!("{}", err.source().unwrap());
                assert!(description.contains("read error"), description);
            }
            res => panic!("expected HttpStream error, got {:?}", res),
        }

        let interrupted_after_field =
            one_chunk(b"message: hello\n").chain(stream::poll_fn(|_cx| {
                Ready(Some(Err(dummy_stream_error("read error"))))
            }));
        match Decoded::new(interrupted_after_field).poll_next_unpin(&mut cx) {
            Ready(Some(Err(err))) => {
                assert!(err.is_http_stream_error());
                let description = format!("{}", err.source().unwrap());
                assert!(description.contains("read error"), description);
            }
            res => panic!("expected HttpStream error, got {:?}", res),
        }

        let interrupted_after_event =
            one_chunk(b"message: hello\n\n").chain(stream::poll_fn(|_cx| {
                Ready(Some(Err(dummy_stream_error("read error"))))
            }));
        let mut decoded = Decoded::new(interrupted_after_event);
        assert_eq!(
            decoded.poll_next_unpin(&mut cx),
            Ready(Some(Ok(event("", &btreemap! {"message" => &b"hello"[..]}))))
        );
        match decoded.poll_next_unpin(&mut cx) {
            Ready(Some(Err(err))) => {
                assert!(err.is_http_stream_error());
                let description = format!("{}", err.source().unwrap());
                assert!(description.contains("read error"), description);
            }
            res => panic!("expected HttpStream error, got {:?}", res),
        }

        let mut decoded = Decoded::new(one_chunk(b"event: \x80\n\n"));
        match decoded.poll_next_unpin(&mut cx) {
            Ready(Some(Err(InvalidEventType(_)))) => (),
            res => panic!("expected InvalidEventType error, got {:?}", res),
        }

        let mut decoded = Decoded::new(one_chunk(b"\x80: invalid UTF-8\n\n"));
        match decoded.poll_next_unpin(&mut cx) {
            Ready(Some(Err(InvalidLine(_)))) => (),
            res => panic!("expected InvalidLine error, got {:?}", res),
        }
    }

    #[test_case("one-event.sse"; "one-event.sse")]
    #[test_case("one-event-crlf.sse"; "one-event-crlf.sse")]
    fn test_decode_one_event(file: &str) {
        let stream = one_chunk_from_file(file);
        let mut decoded = Decoded::new(stream);

        let event = next_event(&mut decoded);
        assert_eq!(event.event_type, "patch");
        let data = event_data(&event).expect("event data should parse");
        assert!(data.contains(r#"path":"/flags/goals.02.featureWithGoals"#));

        assert_eof(decoded);
    }

    #[test_case("two-events.sse"; "two-events.sse")]
    #[test_case("two-events-crlf.sse"; "two-events-crlf.sse")]
    fn test_decode_two_events(file: &str) {
        let stream = one_chunk_from_file(file);
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

    #[test_case("big-event-followed-by-another.sse"; "big-event-followed-by-another.sse")]
    #[test_case("big-event-followed-by-another-crlf.sse"; "big-event-followed-by-another-crlf.sse")]
    fn test_decode_big_event_followed_by_another(file: &str) {
        let stream = one_chunk_from_file(file);
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

    fn one_chunk_from_file(name: &str) -> impl Stream<Item = Result<Bytes>> + Unpin {
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
        S: Stream<Item = Result<Bytes>> + Unpin,
    {
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        match s.poll_next_unpin(&mut cx) {
            Poll::Ready(None) => (),
            Poll::Ready(Some(event)) => panic!(format!("expected eof, got {:?}", event)),
            Poll::Pending => panic!("expected eof, got Pending"),
        }
    }

    fn next_event<S>(s: &mut Decoded<S>) -> Event
    where
        S: Stream<Item = Result<Bytes>> + Unpin,
    {
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        match s.poll_next_unpin(&mut cx) {
            Poll::Ready(Some(Ok(event))) => event,
            Poll::Ready(Some(Err(e))) => panic!(format!("expected eof, got error: {:?}", e)),
            Poll::Ready(None) => panic!("expected event, got eof"),
            Poll::Pending => panic!("expected event, got Pending"),
        }
    }
}
