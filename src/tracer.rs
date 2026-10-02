//! A JSONL trace of what satellite sees and does, for replaying it against another build
//! (feature `trace`; format: docs/window-model/trace-schema.md). Lines are written as
//! events happen and flushed at every batch end, so a killed satellite loses at most one
//! batch.

use log::warn;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

struct Tracer {
    out: BufWriter<File>,
    start: Instant,
}

static TRACER: OnceLock<Option<Mutex<Tracer>>> = OnceLock::new();

fn path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("XWLS_TRACE") {
        return Some(path.into());
    }
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
        })?;
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    Some(
        base.join("xwayland-satellite")
            .join(format!("trace-{secs}-{}.jsonl", std::process::id())),
    )
}

fn open() -> Option<Mutex<Tracer>> {
    let path = path()?;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let file = match File::create(&path) {
        Ok(file) => file,
        Err(err) => {
            warn!("not tracing: couldn't create {}: {err}", path.display());
            return None;
        }
    };
    let mut tracer = Tracer {
        out: BufWriter::new(file),
        start: Instant::now(),
    };
    let mut header = Line::new(0, "start");
    header
        .s("version", crate::version())
        .u("pid", std::process::id().into());
    let _ = writeln!(tracer.out, "{}", header.finish());
    Some(Mutex::new(tracer))
}

/// Writes a line of `kind` with the fields `fill` adds; flushes at `batch_end`.
pub(crate) fn emit(kind: &str, fill: impl FnOnce(&mut Line)) {
    let Some(tracer) = TRACER.get_or_init(open) else {
        return;
    };
    let Ok(mut tracer) = tracer.lock() else {
        return;
    };
    let mut line = Line::new(tracer.start.elapsed().as_millis() as u64, kind);
    fill(&mut line);
    let _ = writeln!(tracer.out, "{}", line.finish());
    if kind == "batch_end" {
        let _ = tracer.out.flush();
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
