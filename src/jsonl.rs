// encode runs only with the trace feature, decode only in the replay tests.
#![allow(dead_code)]

/// A value of the trace's flat JSON objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    Ints(Vec<i64>),
    Strs(Vec<String>),
}

/// Parses one trace line: a JSON object whose values are null, booleans, integers,
/// strings, or arrays of integers or of strings (all the trace schema uses).
pub(crate) fn parse_object(line: &str) -> Result<Vec<(String, Value)>, String> {
    let mut p = Parser {
        s: line.as_bytes(),
        i: 0,
    };
    p.expect(b'{')?;
    let mut fields = Vec::new();
    if p.peek() == Some(b'}') {
        p.i += 1;
        p.end()?;
        return Ok(fields);
    }
    loop {
        let key = p.string()?;
        p.expect(b':')?;
        fields.push((key, p.value()?));
        match p.next() {
            Some(b',') => continue,
            Some(b'}') => {
                p.end()?;
                return Ok(fields);
            }
            other => return Err(format!("expected , or }} at {}, got {other:?}", p.i)),
        }
    }
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn end(&self) -> Result<(), String> {
        if self.i == self.s.len() {
            Ok(())
        } else {
            Err(format!("trailing data at {}", self.i))
        }
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let c = self.peek();
        self.i += 1;
        c
    }

    fn expect(&mut self, c: u8) -> Result<(), String> {
        match self.next() {
            Some(got) if got == c => Ok(()),
            got => Err(format!(
                "expected {:?} at {}, got {got:?}",
                c as char, self.i
            )),
        }
    }

    fn literal(&mut self, word: &str, value: Value) -> Result<Value, String> {
        if self.s[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(value)
        } else {
            Err(format!("bad literal at {}", self.i))
        }
    }

    fn int(&mut self) -> Result<i64, String> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        let digits = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        if self.i > digits + 1 && self.s[digits] == b'0' {
            return Err(format!("leading zero at {start}"));
        }
        std::str::from_utf8(&self.s[start..self.i])
            .ok()
            .and_then(|n| n.parse().ok())
            .ok_or_else(|| format!("bad number at {start}"))
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut out = Vec::new();
        loop {
            match self.next() {
                None => return Err("unterminated string".into()),
                Some(b'"') => break,
                Some(b'\\') => match self.next() {
                    Some(b'"') => out.push(b'"'),
                    Some(b'\\') => out.push(b'\\'),
                    Some(b'n') => out.push(b'\n'),
                    Some(b'r') => out.push(b'\r'),
                    Some(b't') => out.push(b'\t'),
                    Some(b'u') => {
                        let hex = self.s.get(self.i..self.i + 4).ok_or("short \\u escape")?;
                        let code = u32::from_str_radix(
                            std::str::from_utf8(hex).map_err(|e| e.to_string())?,
                            16,
                        )
                        .map_err(|e| e.to_string())?;
                        self.i += 4;
                        let c = char::from_u32(code).ok_or("bad \\u escape")?;
                        let mut buf = [0; 4];
                        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                    }
                    other => return Err(format!("bad escape {other:?}")),
                },
                Some(c) if c < 0x20 => return Err("unescaped control character".into()),
                Some(c) => out.push(c),
            }
        }
        String::from_utf8(out).map_err(|e| e.to_string())
    }

    fn value(&mut self) -> Result<Value, String> {
        match self.peek() {
            Some(b'n') => self.literal("null", Value::Null),
            Some(b't') => self.literal("true", Value::Bool(true)),
            Some(b'f') => self.literal("false", Value::Bool(false)),
            Some(b'"') => Ok(Value::Str(self.string()?)),
            Some(b'[') => {
                self.i += 1;
                if self.peek() == Some(b']') {
                    self.i += 1;
                    return Ok(Value::Ints(Vec::new()));
                }
                if self.peek() == Some(b'"') {
                    let mut items = Vec::new();
                    loop {
                        items.push(self.string()?);
                        match self.next() {
                            Some(b',') => continue,
                            Some(b']') => return Ok(Value::Strs(items)),
                            got => return Err(format!("bad array at {}, got {got:?}", self.i)),
                        }
                    }
                }
                let mut items = Vec::new();
                loop {
                    items.push(self.int()?);
                    match self.next() {
                        Some(b',') => continue,
                        Some(b']') => return Ok(Value::Ints(items)),
                        got => return Err(format!("bad array at {}, got {got:?}", self.i)),
                    }
                }
            }
            _ => Ok(Value::Int(self.int()?)),
        }
    }
}

/// One JSON object, built field by field in order.
pub(crate) struct Line(String);

impl Line {
    pub(crate) fn new(t: u64, kind: &str) -> Self {
        let mut line = Line(format!("{{\"t\":{t}"));
        line.s("k", kind);
        line
    }

    fn key(&mut self, key: &str) {
        self.0.push_str(",\"");
        self.0.push_str(key);
        self.0.push_str("\":");
    }

    fn string(&mut self, value: &str) {
        self.0.push('"');
        for c in value.chars() {
            match c {
                '"' => self.0.push_str("\\\""),
                '\\' => self.0.push_str("\\\\"),
                '\n' => self.0.push_str("\\n"),
                '\r' => self.0.push_str("\\r"),
                '\t' => self.0.push_str("\\t"),
                c if (c as u32) < 0x20 => self.0.push_str(&format!("\\u{:04x}", c as u32)),
                c => self.0.push(c),
            }
        }
        self.0.push('"');
    }

    pub(crate) fn u(&mut self, key: &str, value: u64) -> &mut Self {
        self.key(key);
        self.0.push_str(&value.to_string());
        self
    }

    pub(crate) fn b(&mut self, key: &str, value: bool) -> &mut Self {
        self.key(key);
        self.0.push_str(if value { "true" } else { "false" });
        self
    }

    pub(crate) fn s(&mut self, key: &str, value: &str) -> &mut Self {
        self.key(key);
        self.string(value);
        self
    }

    pub(crate) fn opt_u(&mut self, key: &str, value: Option<u64>) -> &mut Self {
        match value {
            Some(value) => self.u(key, value),
            None => {
                self.key(key);
                self.0.push_str("null");
                self
            }
        }
    }

    pub(crate) fn opt_s(&mut self, key: &str, value: Option<&str>) -> &mut Self {
        match value {
            Some(value) => self.s(key, value),
            None => {
                self.key(key);
                self.0.push_str("null");
                self
            }
        }
    }

    pub(crate) fn ints(&mut self, key: &str, values: &[i64]) -> &mut Self {
        self.key(key);
        let items: Vec<String> = values.iter().map(i64::to_string).collect();
        self.0.push('[');
        self.0.push_str(&items.join(","));
        self.0.push(']');
        self
    }

    pub(crate) fn opt_ints(&mut self, key: &str, values: Option<&[i64]>) -> &mut Self {
        match values {
            Some(values) => self.ints(key, values),
            None => {
                self.key(key);
                self.0.push_str("null");
                self
            }
        }
    }

    pub(crate) fn strs(&mut self, key: &str, values: &[&str]) -> &mut Self {
        self.key(key);
        self.0.push('[');
        for (i, value) in values.iter().enumerate() {
            if i > 0 {
                self.0.push(',');
            }
            self.string(value);
        }
        self.0.push(']');
        self
    }

    pub(crate) fn finish(mut self) -> String {
        self.0.push('}');
        self.0
    }
}

#[cfg(test)]
mod parser_tests {
    use super::{Value, parse_object};

    #[test]
    fn flat_object_values() {
        assert_eq!(
            parse_object(
                r#"{"t":1,"k":"x","a":[1,-2],"b":["c","d"],"e":null,"f":true,"g":"h\"\\\u0001"}"#
            ),
            Ok(vec![
                ("t".into(), Value::Int(1)),
                ("k".into(), Value::Str("x".into())),
                ("a".into(), Value::Ints(vec![1, -2])),
                ("b".into(), Value::Strs(vec!["c".into(), "d".into()])),
                ("e".into(), Value::Null),
                ("f".into(), Value::Bool(true)),
                ("g".into(), Value::Str("h\"\\\u{1}".into())),
            ])
        );
    }

    #[test]
    fn empty_object_and_array() {
        assert_eq!(parse_object("{}"), Ok(vec![]));
        assert_eq!(
            parse_object(r#"{"a":[]}"#),
            Ok(vec![("a".into(), Value::Ints(vec![]))])
        );
    }

    #[test]
    fn malformed_lines() {
        for line in [
            "",
            "{",
            "{\"t\":1,",
            "{\"x\":}",
            "{\"x\":tru}",
            "{}junk",
            "{\"x\":1}junk",
            "{\"x\":01}",
            "{\"x\":\"\n\"}",
            "{\"x\":1.2}",
            "{\"x\":[1,\"a\"]}",
            "{\"x\":[\"a\",1]}",
            "{\"x\":{}}",
            "{\"x\":\"abc}",
            "{\"x\":\"\\q\"}",
            "{\"x\":\"\\u00\"}",
            "{\"x\":\"\\uxxxx\"}",
        ] {
            assert!(parse_object(line).is_err(), "{line}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Line;

    #[test]
    fn line_format() {
        let mut l = Line::new(42, "map");
        l.u("w", 7)
            .b("or", false)
            .strs("types", &["NORMAL", "OTHER"])
            .opt_u("motif_f", None)
            .opt_s("class", Some("a\"b\\c\n"))
            .ints("geom", &[1, -2, 3, 4]);
        assert_eq!(
            l.finish(),
            r#"{"t":42,"k":"map","w":7,"or":false,"types":["NORMAL","OTHER"],"motif_f":null,"class":"a\"b\\c\n","geom":[1,-2,3,4]}"#
        );
    }

    #[test]
    fn control_characters_are_escaped() {
        let mut l = Line::new(0, "x");
        l.s("s", "\u{1}\t");
        assert_eq!(l.finish(), r#"{"t":0,"k":"x","s":"\u0001\t"}"#);
    }
}
