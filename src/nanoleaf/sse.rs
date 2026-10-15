//! Minimal Server-Sent Events parser.
//!
//! Follows the line format of the SSE specification
//! (<https://html.spec.whatwg.org/multipage/server-sent-events.html#event-stream-interpretation>):
//! lines end in LF, CRLF or CR; an empty line dispatches a frame; lines starting with `:` are
//! comments; `id` and `data` are the fields used, all others are ignored. Unlike the
//! specification, the `id` is kept per frame and not carried over: the Nanoleaf device uses it
//! as the event type of each frame.
//!
//! The parser takes arbitrary chunks, so frames and even line endings may be split anywhere.

/// Longest line or frame data accepted; anything longer is dropped, so a misbehaving peer cannot
/// grow the buffer without bound.
const MAX_LEN: usize = 64 * 1024;

/// One dispatched event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFrame {
    /// The `id` field of the frame, if any.
    pub id: Option<String>,
    /// The `data` lines, joined with `\n`.
    pub data: String,
}

/// Incremental SSE parser. Feed chunks with [`SseParser::push`].
#[derive(Debug, Default)]
pub struct SseParser {
    line: Vec<u8>,
    id: Option<String>,
    data: String,
    has_data: bool,
    /// The previous byte was a CR, so an LF right after it belongs to the same line ending.
    after_cr: bool,
    /// The current line grew beyond [`MAX_LEN`] and is skipped up to its end.
    discarding_line: bool,
    /// The current frame's data grew beyond [`MAX_LEN`] and the frame is dropped.
    oversized_frame: bool,
}

impl SseParser {
    /// A parser at the start of a stream.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds a chunk and returns the frames it completed.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseFrame> {
        let mut frames = Vec::new();
        for &byte in chunk {
            if std::mem::take(&mut self.after_cr) && byte == b'\n' {
                continue;
            }
            match byte {
                b'\n' | b'\r' => {
                    self.after_cr = byte == b'\r';
                    let line = std::mem::take(&mut self.line);
                    if std::mem::take(&mut self.discarding_line) {
                        continue;
                    }
                    self.process_line(&line, &mut frames);
                }
                _ if self.discarding_line => {}
                _ if self.line.len() >= MAX_LEN => {
                    tracing::debug!("event stream line too long, dropped");
                    self.line.clear();
                    self.discarding_line = true;
                }
                _ => self.line.push(byte),
            }
        }
        frames
    }

    fn process_line(&mut self, line: &[u8], frames: &mut Vec<SseFrame>) {
        if line.is_empty() {
            self.dispatch(frames);
            return;
        }
        if line[0] == b':' {
            return;
        }
        let (field, value) = match line.iter().position(|&b| b == b':') {
            Some(colon) => {
                let value = &line[colon + 1..];
                (&line[..colon], value.strip_prefix(b" ").unwrap_or(value))
            }
            None => (line, &[][..]),
        };
        match field {
            b"id" => self.id = Some(String::from_utf8_lossy(value).into_owned()),
            b"data" => {
                if self.data.len() + value.len() >= MAX_LEN {
                    self.oversized_frame = true;
                    return;
                }
                self.data.push_str(&String::from_utf8_lossy(value));
                self.data.push('\n');
                self.has_data = true;
            }
            _ => {}
        }
    }

    fn dispatch(&mut self, frames: &mut Vec<SseFrame>) {
        let id = self.id.take();
        let mut data = std::mem::take(&mut self.data);
        let has_data = std::mem::take(&mut self.has_data);
        if std::mem::take(&mut self.oversized_frame) {
            tracing::debug!("event stream frame too large, dropped");
            return;
        }
        if !has_data {
            return;
        }
        data.pop();
        frames.push(SseFrame { id, data });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTROLLER_SESSION: &[u8] =
        include_bytes!("../../fixtures/nanoleaf/sse-controller-session.txt");
    const COMMAND_SESSION: &[u8] =
        include_bytes!("../../fixtures/nanoleaf/sse-command-session.txt");

    fn frame(id: &str, data: &str) -> SseFrame {
        SseFrame {
            id: Some(id.to_owned()),
            data: data.to_owned(),
        }
    }

    fn parse_all(input: &[u8]) -> Vec<SseFrame> {
        SseParser::new().push(input)
    }

    #[test]
    fn complete_frames() {
        let input = b"id: 1\ndata: {\"events\":[{\"attr\":1,\"value\":true}]}\n\n\
                      id: 3\ndata: {\"events\":[{\"attr\":1,\"value\":\"Blaze\"}]}\n\n";
        assert_eq!(
            parse_all(input),
            vec![
                frame("1", r#"{"events":[{"attr":1,"value":true}]}"#),
                frame("3", r#"{"events":[{"attr":1,"value":"Blaze"}]}"#),
            ]
        );
    }

    #[test]
    fn captured_sessions() {
        let frames = parse_all(CONTROLLER_SESSION);
        assert_eq!(frames.len(), 17);
        assert!(
            frames
                .iter()
                .all(|f| f.id.as_deref() == Some("1") || f.id.as_deref() == Some("3"))
        );
        let frames = parse_all(COMMAND_SESSION);
        assert_eq!(frames.len(), 14);
        assert_eq!(
            frames[2],
            frame(
                "1",
                r#"{"events":[{"attr":3,"value":120},{"attr":6,"value":"hs"}]}"#
            )
        );
    }

    #[test]
    fn split_at_every_byte_position() {
        for input in [
            CONTROLLER_SESSION,
            COMMAND_SESSION,
            b"id: 4\r\ndata: {\"events\":[]}\r\n\r\n".as_slice(),
        ] {
            let expected = parse_all(input);
            assert!(!expected.is_empty());
            for split in 0..=input.len() {
                let mut parser = SseParser::new();
                let mut frames = parser.push(&input[..split]);
                frames.extend(parser.push(&input[split..]));
                assert_eq!(frames, expected, "split at {split}");
            }
            // And one byte at a time.
            let mut parser = SseParser::new();
            let frames: Vec<_> = input.iter().flat_map(|b| parser.push(&[*b])).collect();
            assert_eq!(frames, expected);
        }
    }

    #[test]
    fn keep_alive_and_comment_lines() {
        let input = b":\n: keep-alive\n\nid: 2\n: inside a frame\ndata: x\n\n:ping\n\n";
        assert_eq!(parse_all(input), vec![frame("2", "x")]);
    }

    #[test]
    fn crlf_and_cr_line_endings() {
        let expected = vec![frame("1", "a"), frame("2", "b")];
        assert_eq!(
            parse_all(b"id: 1\r\ndata: a\r\n\r\nid: 2\r\ndata: b\r\n\r\n"),
            expected
        );
        assert_eq!(parse_all(b"id: 1\rdata: a\r\rid: 2\rdata: b\r\r"), expected);
    }

    #[test]
    fn fields_without_space_multiple_data_and_unknown_fields() {
        let input = b"id:7\nevent: x\nretry: 10\ndata:first\ndata: second\nbogus\n\n";
        assert_eq!(parse_all(input), vec![frame("7", "first\nsecond")]);
    }

    #[test]
    fn frame_without_data_or_id() {
        assert_eq!(parse_all(b"id: 1\n\n"), vec![]);
        assert_eq!(
            parse_all(b"data: x\n\n"),
            vec![SseFrame {
                id: None,
                data: "x".to_owned()
            }]
        );
    }

    #[test]
    fn id_does_not_leak_into_next_frame() {
        assert_eq!(
            parse_all(b"id: 1\ndata: a\n\ndata: b\n\n"),
            vec![
                frame("1", "a"),
                SseFrame {
                    id: None,
                    data: "b".to_owned()
                }
            ]
        );
    }

    #[test]
    fn incomplete_frame_is_not_dispatched() {
        let mut parser = SseParser::new();
        assert_eq!(parser.push(b"id: 1\ndata: a\n"), vec![]);
        assert_eq!(parser.push(b"\n"), vec![frame("1", "a")]);
    }

    #[test]
    fn invalid_utf8_does_not_panic() {
        assert_eq!(
            parse_all(b"id: 1\ndata: \xff\xfe\n\n"),
            vec![frame("1", "\u{fffd}\u{fffd}")]
        );
    }

    #[test]
    fn overlong_line_is_dropped() {
        let mut input = b"id: 1\ndata: ".to_vec();
        input.extend(std::iter::repeat_n(b'x', MAX_LEN + 10));
        input.extend(b"\n\nid: 2\ndata: ok\n\n");
        assert_eq!(parse_all(&input), vec![frame("2", "ok")]);
    }

    #[test]
    fn oversized_frame_is_dropped() {
        let mut input = b"id: 1\n".to_vec();
        let line = format!("data: {}\n", "x".repeat(1000));
        for _ in 0..70 {
            input.extend(line.as_bytes());
        }
        input.extend(b"\nid: 2\ndata: ok\n\n");
        assert_eq!(parse_all(&input), vec![frame("2", "ok")]);
    }
}
