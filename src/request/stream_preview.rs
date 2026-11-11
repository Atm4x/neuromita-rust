//! Best-effort, read-only preview of spoken text in an incomplete structured
//! JSON response. The projection never executes or accepts response fields.

use serde_json::Value;

const MAX_NESTING: usize = 32;

pub(super) fn spoken_prefix(json: &str) -> Option<String> {
    let mut reader = Reader { json, at: 0 };
    reader.whitespace();
    reader.take(b'{')?;
    loop {
        reader.whitespace();
        if reader.take(b'}').is_some() {
            return None;
        }
        let (key, true) = reader.string(false)? else {
            return None;
        };
        reader.whitespace();
        reader.take(b':')?;
        reader.whitespace();
        if key == "segments" {
            return reader.segments();
        }
        reader.skip_value()?;
        reader.whitespace();
        reader.take(b',')?;
    }
}

struct Reader<'a> {
    json: &'a str,
    at: usize,
}

impl Reader<'_> {
    fn whitespace(&mut self) {
        while self
            .json
            .as_bytes()
            .get(self.at)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.at += 1;
        }
    }

    fn take(&mut self, expected: u8) -> Option<()> {
        if self.json.as_bytes().get(self.at) == Some(&expected) {
            self.at += 1;
            Some(())
        } else {
            None
        }
    }

    fn string(&mut self, allow_partial: bool) -> Option<(String, bool)> {
        let bytes = self.json.as_bytes();
        let start = self.at;
        self.take(b'"')?;
        let mut safe_end = self.at;
        while self.at < bytes.len() {
            match bytes[self.at] {
                b'"' => {
                    self.at += 1;
                    let value = serde_json::from_str(&self.json[start..self.at]).ok()?;
                    return Some((value, true));
                }
                b'\\' => {
                    let Some(&next) = bytes.get(self.at + 1) else {
                        break;
                    };
                    if next == b'u' {
                        if self.at + 6 > bytes.len() {
                            break;
                        }
                        if !bytes[self.at + 2..self.at + 6]
                            .iter()
                            .all(u8::is_ascii_hexdigit)
                        {
                            return None;
                        }
                        self.at += 6;
                    } else if b"\"\\/bfnrt".contains(&next) {
                        self.at += 2;
                    } else {
                        return None;
                    }
                }
                b if b < 0x20 => return None,
                _ => self.at += 1,
            }
            safe_end = self.at;
        }
        if !allow_partial {
            return None;
        }
        // An unfinished escape (including a UTF-16 surrogate pair) must not
        // become visible as malformed or undecoded JSON.
        let mut end = safe_end;
        loop {
            if self.json.is_char_boundary(end)
                && let Ok(text) =
                    serde_json::from_str::<String>(&format!("{}\"", &self.json[start..end]))
            {
                return Some((text, false));
            }
            if end <= start + 1 {
                return None;
            }
            end -= 1;
        }
    }

    fn skip_value(&mut self) -> Option<()> {
        let start = self.at;
        let bytes = self.json.as_bytes();
        match *bytes.get(self.at)? {
            b'"' => {
                self.string(false)?;
                Some(())
            }
            b'[' | b'{' => {
                let mut stack = vec![if bytes[self.at] == b'[' { b']' } else { b'}' }];
                self.at += 1;
                while self.at < bytes.len() {
                    match bytes[self.at] {
                        b'"' => {
                            self.string(false)?;
                        }
                        b'[' | b'{' => {
                            if stack.len() >= MAX_NESTING {
                                return None;
                            }
                            stack.push(if bytes[self.at] == b'[' { b']' } else { b'}' });
                            self.at += 1;
                        }
                        b']' | b'}' => {
                            if stack.pop()? != bytes[self.at] {
                                return None;
                            }
                            self.at += 1;
                            if stack.is_empty() {
                                serde_json::from_str::<Value>(&self.json[start..self.at]).ok()?;
                                return Some(());
                            }
                        }
                        _ => self.at += 1,
                    }
                }
                None
            }
            _ => {
                while let Some(&byte) = bytes.get(self.at) {
                    if matches!(byte, b',' | b'}' | b']') || byte.is_ascii_whitespace() {
                        break;
                    }
                    self.at += 1;
                }
                serde_json::from_str::<Value>(&self.json[start..self.at]).ok()?;
                Some(())
            }
        }
    }

    fn segments(&mut self) -> Option<String> {
        self.take(b'[')?;
        let mut text = Vec::new();
        loop {
            self.whitespace();
            if self.take(b']').is_some() {
                return join_spoken(text);
            }
            if self.take(b'{').is_none() {
                return join_spoken(text);
            }
            loop {
                self.whitespace();
                if self.take(b'}').is_some() {
                    break;
                }
                let (key, true) = self.string(false)? else {
                    return join_spoken(text);
                };
                self.whitespace();
                if self.take(b':').is_none() {
                    return join_spoken(text);
                }
                self.whitespace();
                if key == "text" {
                    let Some((spoken, complete)) = self.string(true) else {
                        return join_spoken(text);
                    };
                    if !spoken.trim().is_empty() {
                        text.push(spoken);
                    }
                    if !complete {
                        return join_spoken(text);
                    }
                } else {
                    if self.skip_value().is_none() {
                        return join_spoken(text);
                    }
                }
                self.whitespace();
                if self.take(b',').is_none() && self.json.as_bytes().get(self.at) != Some(&b'}') {
                    return join_spoken(text);
                }
            }
            self.whitespace();
            if self.take(b',').is_none() {
                return join_spoken(text);
            }
        }
    }
}

fn join_spoken(segments: Vec<String>) -> Option<String> {
    let text = segments
        .iter()
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streams_partial_text_but_never_reasoning_or_side_effects() {
        let prefix = r#"{"reasoning":"SECRET THINKING","tool_call":{"name":"erase","args":{}},"segments":[{"commands":["DELETE"],"text":"Привет, ми"#;
        assert_eq!(spoken_prefix(prefix).as_deref(), Some("Привет, ми"));
        assert_eq!(
            spoken_prefix(r#"{"reasoning":"SECRET","segments":[{"commands":["DELETE"]}"#),
            None
        );
        assert_eq!(
            spoken_prefix(
                r#"{"custom_fields":{"segments":[{"text":"SECRET"}]},"segments":[{"text":"hello"#
            )
            .as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn split_json_escapes_and_utf16_surrogates_are_safe() {
        assert_eq!(
            spoken_prefix(r#"{"segments":[{"text":"one\"#).as_deref(),
            Some("one")
        );
        assert_eq!(
            spoken_prefix(r#"{"segments":[{"text":"one\nnext"#).as_deref(),
            Some("one\nnext")
        );
        assert_eq!(
            spoken_prefix(r#"{"segments":[{"text":"smile \uD83D"#).as_deref(),
            Some("smile")
        );
        assert_eq!(
            spoken_prefix(r#"{"segments":[{"text":"smile \uD83D\uDE00"#).as_deref(),
            Some("smile 😀")
        );
    }

    #[test]
    fn multiple_segments_preserve_spoken_order() {
        assert_eq!(
            spoken_prefix(r#"{"segments":[{"text":"Hello"},{"emotions":["happy"],"text":"world"#)
                .as_deref(),
            Some("Hello world")
        );
        assert_eq!(spoken_prefix(r#"{"segments":[]}"#), None);
    }

    #[test]
    fn spoken_prefix_survives_later_incomplete_hidden_fields() {
        assert_eq!(
            spoken_prefix(r#"{"segments":[{"text":"Hello","commands":["#).as_deref(),
            Some("Hello")
        );
        assert_eq!(
            spoken_prefix(r#"{"segments":[{"text":"Hello"},"#).as_deref(),
            Some("Hello")
        );
        assert_eq!(
            spoken_prefix(r#"{"segments":[{"text":"Hello"},{"reasoning":"SECRET"#).as_deref(),
            Some("Hello")
        );
    }

    #[test]
    fn does_not_select_unrelated_nested_or_malformed_values() {
        for invalid in [
            r#"{"reasoning":"secret","x":{"segments":[{"text":"SECRET"}]}}"#,
            r#"{"segments":"SECRET"}"#,
            r#"{"segments":[{"text":4}]}"#,
            r#"{"segments":[{"reasoning":"SECRET"}]"#,
            r#"{"noise":{"bad":::},"segments":[{"text":"SECRET"}]"#,
        ] {
            assert_eq!(spoken_prefix(invalid), None, "{invalid}");
        }
    }
}
