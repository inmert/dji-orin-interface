//! Timestamped line logging shared by the binaries. Lines go to stdout unless
//! [`init`] installs another sink (e.g. a log file); each line is written and
//! flushed under one lock, so threads never interleave mid-line.

use std::fmt;
use std::io::{self, Write};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();
static SINK: OnceLock<Mutex<Box<dyn Write + Send>>> = OnceLock::new();

/// Start the clock and, optionally, redirect output. Call once, early in
/// `main`; later calls only start the clock if it has not started yet.
pub fn init(sink: Option<Box<dyn Write + Send>>) {
    START.get_or_init(Instant::now);
    if let Some(s) = sink {
        let _ = SINK.set(Mutex::new(s));
    }
}

/// Write one timestamped line. Prefer the [`logln!`](crate::logln) macro.
pub fn line(args: fmt::Arguments<'_>) {
    let t = START.get_or_init(Instant::now).elapsed();
    let (secs, micros) = (t.as_secs(), t.subsec_micros());
    let stamp = format_args!("[{secs}.{micros:06}]");
    // A failed log write has nowhere better to be reported; drop it.
    if let Some(m) = SINK.get() {
        let mut w = m.lock().unwrap_or_else(|p| p.into_inner());
        let _ = writeln!(w, "{stamp} {args}").and_then(|()| w.flush());
    } else {
        let _ = writeln!(io::stdout().lock(), "{stamp} {args}");
    }
}

/// `logln!("fmt", args...)` — timestamped line via [`log::line`](crate::log::line).
#[macro_export]
macro_rules! logln {
    ($($arg:tt)*) => {
        $crate::log::line(format_args!($($arg)*))
    };
}

/// Formats bytes as space-prefixed hex: `" 55 0d 04"`.
#[derive(Debug, Clone, Copy)]
pub struct Hex<'a>(pub &'a [u8]);

impl fmt::Display for Hex<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, " {b:02x}"))
    }
}

/// Bytes as space-separated hex with no leading space: `"55 0d ff"`.
pub fn hex(bytes: &[u8]) -> String {
    let s = Hex(bytes).to_string();
    s.strip_prefix(' ').unwrap_or(&s).to_owned()
}

#[cfg(test)]
mod tests {
    use super::{Hex, hex};

    #[test]
    fn hex_formats_bytes() {
        assert_eq!(Hex(&[0x55, 0x0d, 0xff]).to_string(), " 55 0d ff");
        assert_eq!(Hex(&[]).to_string(), "");
        assert_eq!(hex(&[0x55, 0x0d, 0xff]), "55 0d ff");
        assert_eq!(hex(&[]), "");
    }
}
