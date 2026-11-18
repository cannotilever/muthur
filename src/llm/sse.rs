//! Server-sent-events framing per the HTML spec.

#[derive(Clone, Debug, PartialEq, Default)]
pub struct SseRecord {
    pub event: Option<String>,
    pub data: String,
}

#[derive(Default)]
pub struct SseDecoder {
    buf: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
}

impl SseDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<SseRecord> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=nl).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8_lossy(&line);
            if line.is_empty() {
                if !self.data.is_empty() {
                    out.push(SseRecord { event: self.event.take(), data: self.data.join("\n") });
                }
                self.event = None;
                self.data.clear();
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = match line.split_once(':') {
                Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
                None => (&*line, ""),
            };
            match field {
                "event" => self.event = Some(value.to_string()),
                "data" => self.data.push(value.to_string()),
                _ => {}
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn splits_records_on_blank_line() { let mut d = SseDecoder::default(); let r = d.push(b"event: a\ndata: {\"x\":1}\n\ndata: two\n\n"); assert_eq!(r, vec![SseRecord{event:Some("a".into()),data:"{\"x\":1}".into()}, SseRecord{event:None,data:"two".into()}]); }
    #[test] fn handles_crlf_comments_and_multiline() { let mut d = SseDecoder::default(); assert_eq!(d.push(b": ping\r\ndata: a\r\ndata: b\r\n\r\n"), vec![SseRecord{event:None,data:"a\nb".into()}]); }
    #[test] fn arbitrary_splits_including_utf8() { let all = "data: caf\u{e9} \u{2014}\n\n".as_bytes(); for cut in 0..all.len() { let mut d = SseDecoder::default(); let mut r = d.push(&all[..cut]); r.extend(d.push(&all[cut..])); assert_eq!(r, vec![SseRecord{event:None,data:"caf\u{e9} \u{2014}".into()}], "cut {cut}"); } }
    #[test] fn done_sentinel_is_plain_data() { let mut d = SseDecoder::default(); assert_eq!(d.push(b"data: [DONE]\n\n")[0].data, "[DONE]"); }
}
