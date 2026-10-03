//! A JSONL trace of what satellite sees and does, for replaying it against another build
//! (feature `trace`; format: docs/window-model/trace-schema.md). Lines are written as
//! events happen and flushed at every batch end, so a killed satellite loses at most one
//! batch.

use crate::jsonl::Line;
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

/// Writes `line` as it is; flushes at a batch end.
pub(crate) fn emit_line(line: String) {
    let Some(tracer) = TRACER.get_or_init(open) else {
        return;
    };
    let Ok(mut tracer) = tracer.lock() else {
        return;
    };
    let flush = line.contains("\"k\":\"batch_end\"");
    let _ = writeln!(tracer.out, "{line}");
    if flush {
        let _ = tracer.out.flush();
    }
}
