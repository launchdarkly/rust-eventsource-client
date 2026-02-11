use std::{collections::VecDeque, convert::TryFrom, str::from_utf8};

use hyper::body::Bytes;
use log::{debug, log_enabled, trace};
use pin_project::pin_project;

use crate::response::Response;

use super::error::{Error, Result};

#[derive(Default, PartialEq)]
struct EventData {
    pub event_type: String,
    pub data: String,
    pub id: Option<String>,
    pub retry: Option<u64>,
}

impl EventData {
    fn new() -> Self {
        Self::default()
    }

    pub fn append_data(&mut self, value: &str) {
        self.data.push_str(value);
        self.data.push('\n');
    }

    pub fn with_id(mut self, value: Option<String>) -> Self {
        self.id = value;
        self
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum SSE {
    Connected(ConnectionDetails),
    Event(Event),
    Comment(String),
}

impl TryFrom<EventData> for Option<SSE> {
    type Error = Error;

    fn try_from(event_data: EventData) -> std::result::Result<Self, Self::Error> {
        if event_data == EventData::default() {
            return Err(Error::InvalidEvent);
        }

        if event_data.data.is_empty() {
            return Ok(None);
        }

        let event_type = if event_data.event_type.is_empty() {
            String::from("message")
        } else {
            event_data.event_type
        };

        let mut data = event_data.data.clone();
        data.truncate(data.len() - 1);

        let id = event_data.id.clone();

        let retry = event_data.retry;

        Ok(Some(SSE::Event(Event {
            event_type,
            data,
            id,
            retry,
        })))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectionDetails {
    response: Response,
}

impl ConnectionDetails {
    pub(crate) fn new(response: Response) -> Self {
        Self { response }
    }

    /// Returns information describing the response at the time of connection.
    pub fn response(&self) -> &Response {
        &self.response
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Event {
    pub event_type: String,
    pub data: String,
    pub id: Option<String>,
    pub retry: Option<u64>,
}

const LOGIFY_MAX_CHARS: usize = 100;
fn logify(bytes: &[u8]) -> String {
    let stringified = from_utf8(bytes).unwrap_or("<bad utf8>");
    stringified.chars().take(LOGIFY_MAX_CHARS).collect()
}

fn parse_field(line: &[u8]) -> Result<Option<(&str, &str)>> {
    if line.is_empty() {
        return Err(Error::InvalidLine(
            "should never try to parse an empty line (probably a bug)".into(),
        ));
    }

    match line.iter().position(|&b| b':' == b) {
        Some(0) => {
            let value = &line[1..];
            debug!("comment: {}", logify(value));
            Ok(Some(("comment", parse_value(value)?)))
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

            Ok(Some((key, parse_value(value)?)))
        }
        None => Ok(Some((parse_key(line)?, ""))),
    }
}

fn parse_key(key: &[u8]) -> Result<&str> {
    from_utf8(key).map_err(|e| Error::InvalidLine(format!("malformed key: {e:?}")))
}

fn parse_value(value: &[u8]) -> Result<&str> {
    from_utf8(value).map_err(|e| Error::InvalidLine(format!("malformed value: {e:?}")))
}

// A state machine for handling the BOM header.
#[derive(Debug)]
enum BomHeaderState {
    Parsing(Vec<u8>),
    Consumed,
}

const BOM_HEADER: &[u8] = b"\xEF\xBB\xBF";

// Try to consume the BOM header from the given bytes.
// If the BOM header is found, return the remaining bytes, otherwise return the origin buffer.
// Return `None` if we cannot determine whether the BOM header is present.
fn try_consume_bom_header(buf: &[u8]) -> Option<&[u8]> {
    if buf.len() < BOM_HEADER.len() {
        if BOM_HEADER.starts_with(buf) {
            None
        } else {
            Some(buf)
        }
    } else if buf.starts_with(BOM_HEADER) {
        Some(&buf[BOM_HEADER.len()..])
    } else {
        Some(buf)
    }
}

#[pin_project]
#[must_use = "streams do nothing unless polled"]
pub struct EventParser {
    /// buffer for lines we know are complete (terminated) but not yet parsed into event fields, in
    /// the order received
    complete_lines: VecDeque<Vec<u8>>,
    /// buffer for the most-recently received line, pending completion (by a newline terminator) or
    /// extension (by more non-newline bytes)
    incomplete_line: Option<Vec<u8>>,
    /// flagged if the last character processed as a carriage return; used to help process CRLF
    /// pairs
    last_char_was_cr: bool,
    /// the event data currently being decoded
    event_data: Option<EventData>,
    /// the last-seen event ID; events without an ID will take on this value until it is updated.
    last_event_id: Option<String>,
    sse: VecDeque<SSE>,
    /// state machine for handling the BOM header
    bom_header_state: BomHeaderState,
}

impl EventParser {
    pub fn new() -> Self {
        Self {
            complete_lines: VecDeque::with_capacity(10),
            incomplete_line: None,
            last_char_was_cr: false,
            event_data: None,
            last_event_id: None,
            sse: VecDeque::with_capacity(3),
            bom_header_state: BomHeaderState::Parsing(Vec::new()),
        }
    }

    pub fn was_processing(&self) -> bool {
        if self.incomplete_line.is_some() || !self.complete_lines.is_empty() {
            true
        } else {
            !self.sse.is_empty()
        }
    }

    pub fn get_event(&mut self) -> Option<SSE> {
        self.sse.pop_front()
    }

    pub fn process_bytes(&mut self, bytes: Bytes) -> Result<()> {
        trace!("Parsing bytes {bytes:?}");

        // According to the SSE spec, a BOM header may be present at the beginning of the stream,
        // which must be stripped before the message processing.
        let bytes_to_process =
            if let BomHeaderState::Parsing(header_buf) = &mut self.bom_header_state {
                header_buf.extend_from_slice(&bytes);
                if let Some(rest) = try_consume_bom_header(header_buf) {
                    let owned_rest = rest.to_vec();
                    self.bom_header_state = BomHeaderState::Consumed;
                    // Once the BOM header is consumed, we can process the rest of the bytes.
                    Bytes::from_owner(owned_rest)
                } else {
                    return Ok(());
                }
            } else {
                bytes
            };

        // We get bytes from the underlying stream in chunks.  Decoding a chunk has two phases:
        // decode the chunk into lines, and decode the lines into events.
        //
        // We counterintuitively do these two phases in reverse order. Because both lines and
        // events may be split across chunks, we need to ensure we have a complete
        // (newline-terminated) line before parsing it, and a complete event
        // (empty-line-terminated) before returning it. So we buffer lines between poll()
        // invocations, and begin by processing any incomplete events from previous invocations,
        // before requesting new input from the underlying stream and processing that.
        self.decode_and_buffer_lines(bytes_to_process);
        self.parse_complete_lines_into_event()?;

        Ok(())
    }

    // Populate the event fields from the complete lines already seen, until we either encounter an
    // empty line - indicating we've decoded a complete event - or we run out of complete lines to
    // process.
    //
    // Returns the event for dispatch if it is complete.
    fn parse_complete_lines_into_event(&mut self) -> Result<()> {
        loop {
            let mut seen_empty_line = false;

            while let Some(line) = self.complete_lines.pop_front() {
                if line.is_empty() && self.event_data.is_some() {
                    seen_empty_line = true;
                    break;
                } else if line.is_empty() {
                    continue;
                }

                if let Some((key, value)) = parse_field(&line)? {
                    if key == "comment" {
                        self.sse.push_back(SSE::Comment(value.to_string()));
                        continue;
                    }

                    let id = &self.last_event_id;
                    let event_data = self
                        .event_data
                        .get_or_insert_with(|| EventData::new().with_id(id.clone()));

                    if key == "event" {
                        event_data.event_type = value.to_string()
                    } else if key == "data" {
                        event_data.append_data(value);
                    } else if key == "id" {
                        // If id contains a null byte, it is a non-fatal error and the rest of
                        // the event should be parsed if possible.
                        if value.chars().any(|c| c == '\0') {
                            debug!("Ignoring event ID containing null byte");
                            continue;
                        }

                        if value.is_empty() {
                            self.last_event_id = Some("".to_string());
                        } else {
                            self.last_event_id = Some(value.to_string());
                        }

                        event_data.id.clone_from(&self.last_event_id)
                    } else if key == "retry" {
                        match value.parse::<u64>() {
                            Ok(retry) => {
                                event_data.retry = Some(retry);
                            }
                            _ => debug!("Failed to parse {value:?} into retry value"),
                        };
                    }
                }
            }

            if seen_empty_line {
                let event_data = self.event_data.take();

                trace!(
                    "seen empty line, event_data is {:?})",
                    event_data.as_ref().map(|event_data| &event_data.event_type)
                );

                if let Some(event_data) = event_data {
                    match Option::<SSE>::try_from(event_data) {
                        Err(e) => return Err(e),
                        Ok(None) => (),
                        Ok(Some(event)) => self.sse.push_back(event),
                    };
                }

                continue;
            } else {
                trace!("processed all complete lines but event_data not yet complete");
            }

            break;
        }

        Ok(())
    }

    // Decode a chunk into lines and buffer them for subsequent parsing, taking account of
    // incomplete lines from previous chunks.
    fn decode_and_buffer_lines(&mut self, chunk: Bytes) {
        let mut lines = chunk.split_inclusive(|&b| b == b'\n' || b == b'\r');
        // The first and last elements in this split are special. The spec requires lines to be
        // terminated. But lines may span chunks, so:
        //  * the last line, if non-empty (i.e. if chunk didn't end with a line terminator),
        //    should be buffered as an incomplete line
        //  * the first line should be appended to the incomplete line, if any

        if let Some(incomplete_line) = self.incomplete_line.as_mut() {
            if let Some(line) = lines.next() {
                trace!(
                    "extending line from previous chunk: {:?}+{:?}",
                    logify(incomplete_line),
                    logify(line)
                );

                self.last_char_was_cr = false;
                if !line.is_empty() {
                    // Checking the last character handles lines where the last character is a
                    // terminator, but also where the entire line is a terminator.
                    match line.last().unwrap() {
                        b'\r' => {
                            incomplete_line.extend_from_slice(&line[..line.len() - 1]);
                            let il = self.incomplete_line.take();
                            self.complete_lines.push_back(il.unwrap());
                            self.last_char_was_cr = true;
                        }
                        b'\n' => {
                            incomplete_line.extend_from_slice(&line[..line.len() - 1]);
                            let il = self.incomplete_line.take();
                            self.complete_lines.push_back(il.unwrap());
                        }
                        _ => incomplete_line.extend_from_slice(line),
                    };
                }
            }
        }

        let mut lines = lines.peekable();
        while let Some(line) = lines.next() {
            if let Some(actually_complete_line) = self.incomplete_line.take() {
                // we saw the next line, so the previous one must have been complete after all
                trace!(
                    "previous line was complete: {:?}",
                    logify(&actually_complete_line)
                );
                self.complete_lines.push_back(actually_complete_line);
            }

            if self.last_char_was_cr && line == [b'\n'] {
                // This is a continuation of a \r\n pair, so we can ignore this line. We do need to
                // reset our flag though.
                self.last_char_was_cr = false;
                continue;
            }

            self.last_char_was_cr = false;
            if line.ends_with(b"\r") {
                self.complete_lines
                    .push_back(line[..line.len() - 1].to_vec());
                self.last_char_was_cr = true;
            } else if line.ends_with(b"\n") {
                // self isn't a continuation, but rather a line ending with a LF terminator.
                self.complete_lines
                    .push_back(line[..line.len() - 1].to_vec());
            } else if line.is_empty() {
                // this is the last line and it's empty, no need to buffer it
                trace!("chunk ended with a line terminator");
            } else if lines.peek().is_some() {
                // this line isn't the last and we know from previous checks it doesn't end in a
                // terminator, so we can consider it complete
                self.complete_lines.push_back(line.to_vec());
            } else {
                // last line needs to be buffered as it may be incomplete
                trace!("buffering incomplete line: {:?}", logify(line));
                self.incomplete_line = Some(line.to_vec());
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

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{Error::*, *};
    use proptest::proptest;
    use test_case::test_case;

    fn field<'a>(key: &'a str, value: &'a str) -> Result<Option<(&'a str, &'a str)>> {
        Ok(Some((key, value)))
    }

    /// Requires an event to be popped from the given parser.
    /// Event properties can be asserted using a closure.
    fn require_pop_event<F>(parser: &mut EventParser, f: F)
    where
        F: FnOnce(Event),
    {
        if let Some(SSE::Event(event)) = parser.get_event() {
            f(event)
        } else {
            panic!("Event should have been received")
        }
    }

    #[test]
    fn test_logify_handles_code_point_boundaries() {
        let phase = String::from_str(
            "这是一条很长的消息，最初导致我们的代码出现恐慌。我希望情况不再如此。这是一条很长的消息，最初导致我们的代码出现恐慌。我希望情况不再如此。这是一条很长的消息，最初导致我们的代码出现恐慌。我希望情况不再如此。这是一条很长的消息，最初导致我们的代码出现恐慌。我希望情况不再如此。",
        )
        .expect("Invalid sample string");

        let input: &[u8] = phase.as_bytes();
        let result = logify(input);

        assert!(result == "这是一条很长的消息，最初导致我们的代码出现恐慌。我希望情况不再如此。这是一条很长的消息，最初导致我们的代码出现恐慌。我希望情况不再如此。这是一条很长的消息，最初导致我们的代码出现恐慌。我希望情况不再如");
    }

    #[test]
    fn test_parse_field_invalid() {
        assert!(parse_field(b"").is_err());

        match parse_field(b"\x80: invalid UTF-8") {
            Err(InvalidLine(msg)) => assert!(msg.contains("Utf8Error")),
            res => panic!("expected InvalidLine error, got {res:?}"),
        }
    }

    #[test]
    fn test_event_id_error_if_invalid_utf8() {
        let mut bytes = Vec::from("id: ");
        let mut invalid = vec![b'\xf0', b'\x28', b'\x8c', b'\xbc'];
        bytes.append(&mut invalid);
        bytes.push(b'\n');
        let mut parser = EventParser::new();
        assert!(parser.process_bytes(Bytes::from(bytes)).is_err());
    }

    #[test]
    fn test_parse_field_comments() {
        assert_eq!(parse_field(b":"), field("comment", ""));
        assert_eq!(
            parse_field(b":hello \0 world"),
            field("comment", "hello \0 world")
        );
        assert_eq!(parse_field(b":event: foo"), field("comment", "event: foo"));
    }

    #[test]
    fn test_parse_field_valid() {
        assert_eq!(parse_field(b"event:foo"), field("event", "foo"));
        assert_eq!(parse_field(b"event: foo"), field("event", "foo"));
        assert_eq!(parse_field(b"event:  foo"), field("event", " foo"));
        assert_eq!(parse_field(b"event:\tfoo"), field("event", "\tfoo"));
        assert_eq!(parse_field(b"event: foo "), field("event", "foo "));

        assert_eq!(parse_field(b"disconnect:"), field("disconnect", ""));
        assert_eq!(parse_field(b"disconnect: "), field("disconnect", ""));
        assert_eq!(parse_field(b"disconnect:  "), field("disconnect", " "));
        assert_eq!(parse_field(b"disconnect:\t"), field("disconnect", "\t"));

        assert_eq!(parse_field(b"disconnect"), field("disconnect", ""));

        assert_eq!(parse_field(b" : foo"), field(" ", "foo"));
        assert_eq!(parse_field(b"\xe2\x98\x83: foo"), field("☃", "foo"));
    }

    fn event(typ: &str, data: &str) -> SSE {
        SSE::Event(Event {
            data: data.to_string(),
            id: None,
            event_type: typ.to_string(),
            retry: None,
        })
    }

    fn event_with_id(typ: &str, data: &str, id: &str) -> SSE {
        SSE::Event(Event {
            data: data.to_string(),
            id: Some(id.to_string()),
            event_type: typ.to_string(),
            retry: None,
        })
    }

    #[test]
    fn test_event_without_data_yields_no_event() {
        let mut parser = EventParser::new();
        assert!(parser.process_bytes(Bytes::from("id: abc\n\n")).is_ok());
        assert!(parser.get_event().is_none());
    }

    #[test]
    fn test_ignore_id_containing_null() {
        let mut parser = EventParser::new();
        assert!(parser
            .process_bytes(Bytes::from("id: a\x00bc\nevent: add\ndata: abc\n\n"))
            .is_ok());

        if let Some(SSE::Event(event)) = parser.get_event() {
            assert!(event.id.is_none());
        } else {
            panic!("Event should have been received");
        }
    }

    #[test_case("event: add\ndata: hello\n\n", "add".into())]
    #[test_case("data: hello\n\n", "message".into())]
    fn test_event_can_parse_type_correctly(chunk: &'static str, event_type: String) {
        let mut parser = EventParser::new();

        assert!(parser.process_bytes(Bytes::from(chunk)).is_ok());

        require_pop_event(&mut parser, |e| assert_eq!(event_type, e.event_type));
    }

    #[test_case("data: hello\n\n", event("message", "hello"); "parses event body with LF")]
    #[test_case("data: hello\n\r", event("message", "hello"); "parses event body with LF and trailing CR")]
    #[test_case("data: hello\r\n\n", event("message", "hello"); "parses event body with CRLF")]
    #[test_case("data: hello\r\n\r", event("message", "hello"); "parses event body with CRLF and trailing CR")]
    #[test_case("data: hello\r\r", event("message", "hello"); "parses event body with CR")]
    #[test_case("data: hello\r\r\n", event("message", "hello"); "parses event body with CR and trailing CRLF")]
    #[test_case("id: 1\ndata: hello\n\n", event_with_id("message", "hello", "1"))]
    #[test_case("id: 😀\ndata: hello\n\n", event_with_id("message", "hello", "😀"))]
    fn test_decode_chunks_simple(chunk: &'static str, event: SSE) {
        let mut parser = EventParser::new();
        assert!(parser.process_bytes(Bytes::from(chunk)).is_ok());
        assert_eq!(parser.get_event().unwrap(), event);
        assert!(parser.get_event().is_none());
    }

    #[test_case("persistent-event-id.sse"; "persistent-event-id.sse")]
    fn test_last_id_persists_if_not_overridden(file: &str) {
        let contents = read_contents_from_file(file);
        let mut parser = EventParser::new();
        assert!(parser.process_bytes(Bytes::from(contents)).is_ok());

        require_pop_event(&mut parser, |e| assert_eq!(e.id, Some("1".into())));
        require_pop_event(&mut parser, |e| assert_eq!(e.id, Some("1".into())));
        require_pop_event(&mut parser, |e| assert_eq!(e.id, Some("3".into())));
        require_pop_event(&mut parser, |e| assert_eq!(e.id, Some("3".into())));
    }

    #[test_case(b":hello\n"; "with LF")]
    #[test_case(b":hello\r"; "with CR")]
    #[test_case(b":hello\r\n"; "with CRLF")]
    fn test_decode_chunks_comments_are_generated(chunk: &'static [u8]) {
        let mut parser = EventParser::new();
        assert!(parser.process_bytes(Bytes::from(chunk)).is_ok());
        assert!(parser.get_event().is_some());
    }

    #[test]
    fn test_comment_is_separate_from_event() {
        let mut parser = EventParser::new();
        let result = parser.process_bytes(Bytes::from(":comment\ndata:hello\n\n"));
        assert!(result.is_ok());

        let comment = parser.get_event();
        assert!(matches!(comment, Some(SSE::Comment(_))));

        let event = parser.get_event();
        assert!(matches!(event, Some(SSE::Event(_))));

        assert!(parser.get_event().is_none());
    }

    #[test]
    fn test_comment_with_trailing_blank_line() {
        let mut parser = EventParser::new();
        let result = parser.process_bytes(Bytes::from(":comment\n\r\n\r"));
        assert!(result.is_ok());

        let comment = parser.get_event();
        assert!(matches!(comment, Some(SSE::Comment(_))));

        assert!(parser.get_event().is_none());
    }

    #[test_case(&["data:", "hello\n\n"], event("message", "hello"); "data split")]
    #[test_case(&["data:hell", "o\n\n"], event("message", "hello"); "data truncated")]
    fn test_decode_message_split_across_chunks(chunks: &[&'static str], event: SSE) {
        let mut parser = EventParser::new();

        if let Some((last, chunks)) = chunks.split_last() {
            for chunk in chunks {
                assert!(parser.process_bytes(Bytes::from(*chunk)).is_ok());
                assert!(parser.get_event().is_none());
            }

            assert!(parser.process_bytes(Bytes::from(*last)).is_ok());
            assert_eq!(parser.get_event(), Some(event));
            assert!(parser.get_event().is_none());
        } else {
            panic!("Failed to split last");
        }
    }

    #[test_case(&["data:hell", "o\n\ndata:", "world\n\n"], &[event("message", "hello"), event("message", "world")]; "with lf")]
    #[test_case(&["data:hell", "o\r\rdata:", "world\r\r"], &[event("message", "hello"), event("message", "world")]; "with cr")]
    #[test_case(&["data:hell", "o\r\n\ndata:", "world\r\n\n"], &[event("message", "hello"), event("message", "world")]; "with crlf")]
    fn test_decode_multiple_messages_split_across_chunks(chunks: &[&'static str], events: &[SSE]) {
        let mut parser = EventParser::new();

        for chunk in chunks {
            assert!(parser.process_bytes(Bytes::from(*chunk)).is_ok());
        }

        for event in events {
            assert_eq!(parser.get_event().unwrap(), *event);
        }

        assert!(parser.get_event().is_none());
    }

    #[test]
    fn test_decode_line_split_across_chunks() {
        let mut parser = EventParser::new();
        assert!(parser.process_bytes(Bytes::from("data:foo")).is_ok());
        assert!(parser.process_bytes(Bytes::from("")).is_ok());
        assert!(parser.process_bytes(Bytes::from("baz\n\n")).is_ok());
        assert_eq!(parser.get_event(), Some(event("message", "foobaz")));
        assert!(parser.get_event().is_none());

        assert!(parser.process_bytes(Bytes::from("data:foo")).is_ok());
        assert!(parser.process_bytes(Bytes::from("bar")).is_ok());
        assert!(parser.process_bytes(Bytes::from("baz\n\n")).is_ok());
        assert_eq!(parser.get_event(), Some(event("message", "foobarbaz")));
        assert!(parser.get_event().is_none());
    }

    #[test]
    fn test_decode_concatenates_multiple_values_for_same_field() {
        let mut parser = EventParser::new();
        assert!(parser.process_bytes(Bytes::from("data:hello\n")).is_ok());
        assert!(parser.process_bytes(Bytes::from("data:world\n\n")).is_ok());
        assert_eq!(parser.get_event(), Some(event("message", "hello\nworld")));
        assert!(parser.get_event().is_none());
    }

    #[test_case("\n\n\n\n" ; "all LFs")]
    #[test_case("\r\r\r\r" ; "all CRs")]
    #[test_case("\r\n\r\n\r\n\r\n" ; "all CRLFs")]
    fn test_decode_repeated_terminators(chunk: &'static str) {
        let mut parser = EventParser::new();
        assert!(parser.process_bytes(Bytes::from(chunk)).is_ok());

        // spec seems unclear on whether this should actually dispatch empty events, but that seems
        // unhelpful for all practical purposes
        assert!(parser.get_event().is_none());
    }

    #[test]
    fn test_decode_extra_terminators_between_events() {
        let mut parser = EventParser::new();
        assert!(parser
            .process_bytes(Bytes::from("data: abc\n\n\ndata: def\n\n"))
            .is_ok());

        assert_eq!(parser.get_event(), Some(event("message", "abc")));
        assert_eq!(parser.get_event(), Some(event("message", "def")));
        assert!(parser.get_event().is_none());
    }

    #[test_case("one-event.sse"; "one-event.sse")]
    #[test_case("one-event-crlf.sse"; "one-event-crlf.sse")]
    fn test_decode_one_event(file: &str) {
        let contents = read_contents_from_file(file);
        let mut parser = EventParser::new();
        assert!(parser.process_bytes(Bytes::from(contents)).is_ok());

        require_pop_event(&mut parser, |e| {
            assert_eq!(e.event_type, "patch");
            assert!(e
                .data
                .contains(r#"path":"/flags/goals.02.featureWithGoals"#));
        });
    }

    #[test_case("two-events.sse"; "two-events.sse")]
    #[test_case("two-events-crlf.sse"; "two-events-crlf.sse")]
    fn test_decode_two_events(file: &str) {
        let contents = read_contents_from_file(file);
        let mut parser = EventParser::new();
        assert!(parser.process_bytes(Bytes::from(contents)).is_ok());

        require_pop_event(&mut parser, |e| {
            assert_eq!(e.event_type, "one");
            assert_eq!(e.data, "One");
        });

        require_pop_event(&mut parser, |e| {
            assert_eq!(e.event_type, "two");
            assert_eq!(e.data, "Two");
        });
    }

    #[test_case("big-event-followed-by-another.sse"; "big-event-followed-by-another.sse")]
    #[test_case("big-event-followed-by-another-crlf.sse"; "big-event-followed-by-another-crlf.sse")]
    fn test_decode_big_event_followed_by_another(file: &str) {
        let contents = read_contents_from_file(file);
        let mut parser = EventParser::new();
        assert!(parser.process_bytes(Bytes::from(contents)).is_ok());

        require_pop_event(&mut parser, |e| {
            assert_eq!(e.event_type, "patch");
            assert!(e.data.len() > 10_000);
            assert!(e.data.contains(r#"path":"/flags/big.00.bigFeatureKey"#));
        });

        require_pop_event(&mut parser, |e| {
            assert_eq!(e.event_type, "patch");
            assert!(e
                .data
                .contains(r#"path":"/flags/goals.02.featureWithGoals"#));
        });
    }

    fn read_contents_from_file(name: &str) -> Vec<u8> {
        std::fs::read(format!("test-data/{name}"))
            .unwrap_or_else(|_| panic!("couldn't read {name}"))
    }

    #[test]
    fn test_event_parser_with_bom_header_split_across_chunks() {
        let mut parser = EventParser::new();
        // First chunk: partial BOM
        assert!(parser
            .process_bytes(Bytes::from(b"\xEF\xBB".as_slice()))
            .is_ok());
        assert!(parser.get_event().is_none());
        // Second chunk: rest of BOM + data
        assert!(parser
            .process_bytes(Bytes::from(b"\xBFdata: hello\n\n".as_slice()))
            .is_ok());
        assert_eq!(parser.get_event(), Some(event("message", "hello")));
        assert!(parser.get_event().is_none());
    }

    #[test]
    fn test_event_parser_second_bom_should_fail() {
        let mut parser = EventParser::new();
        // First event with BOM - should succeed
        assert!(parser
            .process_bytes(Bytes::from(b"\xEF\xBB\xBFdata: first\n\n".as_slice()))
            .is_ok());
        assert_eq!(parser.get_event(), Some(event("message", "first")));

        // Second event with BOM - should fail (only first message can have BOM)
        let result = parser.process_bytes(Bytes::from(b"\xEF\xBB\xBFdata: second\n\n".as_slice()));
        assert!(result.is_err());
    }

    proptest! {
        #[test]
        fn test_decode_and_buffer_lines_does_not_crash(next in "(\r\n|\r|\n)*event: [^\n\r:]*(\r\n|\r|\n)", previous in "(\r\n|\r|\n)*event: [^\n\r:]*(\r\n|\r|\n)") {
            let mut parser = EventParser::new();
            parser.incomplete_line = Some(previous.as_bytes().to_vec());
            parser.decode_and_buffer_lines(Bytes::from(next));
        }
    }
}
