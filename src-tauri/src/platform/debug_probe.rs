//! Bounded, debug-only timing evidence. No request payloads or business content.
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

const MAX_BYTES: u64 = 8 * 1024 * 1024;
static OUTPUT: Mutex<Option<(std::fs::File, u64)>> = Mutex::new(None);

pub fn initialize(directory: &Path) {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let path = directory.join(format!(
        "worktrace-probe-{}-{stamp}.jsonl",
        std::process::id()
    ));
    let opened = std::fs::create_dir_all(directory).and_then(|_| {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
    });
    match opened {
        Ok(file) => {
            *OUTPUT.lock().unwrap_or_else(|e| e.into_inner()) = Some((file, 0));
            record(&serde_json::json!({"phase": "probe_started", "pid": std::process::id()}));
        }
        Err(error) => eprintln!("[worktrace] probe log unavailable: {error}"),
    }
}

fn append_bounded(writer: &mut impl Write, written: &mut u64, line: &[u8]) -> std::io::Result<()> {
    if line.len() as u64 > MAX_BYTES.saturating_sub(*written) {
        return Ok(());
    }
    writer.write_all(line)?;
    *written += line.len() as u64;
    writer.flush()
}

pub fn record(value: &serde_json::Value) {
    let mut output = OUTPUT.lock().unwrap_or_else(|e| e.into_inner());
    let Some((file, written)) = output.as_mut() else {
        return;
    };
    let line = format!("{value}\n");
    // Disable after an I/O failure rather than changing command behavior or retrying every tick.
    if append_bounded(file, written, line.as_bytes()).is_err() {
        *output = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_sink_keeps_complete_lines_and_stops_at_limit() {
        let mut writer = Vec::new();
        let mut written = MAX_BYTES - 3;
        append_bounded(&mut writer, &mut written, b"{}\n").unwrap();
        append_bounded(&mut writer, &mut written, b"{}\n").unwrap();
        assert_eq!(writer, b"{}\n");
        assert_eq!(written, MAX_BYTES);
    }

    #[test]
    fn failed_write_is_reported_without_claiming_bytes_written() {
        struct FailedWriter;
        impl Write for FailedWriter {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("unavailable"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut written = 0;
        assert!(append_bounded(&mut FailedWriter, &mut written, b"{}\n").is_err());
        assert_eq!(written, 0);
    }
}
