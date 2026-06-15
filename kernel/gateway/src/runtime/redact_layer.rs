#![forbid(unsafe_code)]

// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! A [`std::io::Write`] wrapper that redacts sensitive data from tracing output.
//!
//! Wraps any writer (stdout, file, etc.) and buffers output line-by-line,
//! applying [`kernel::redactor::redact_sensitive_data`] to each complete
//! line before forwarding to the inner writer.
//!
//! This ensures that secrets (API keys, tokens, passwords, JWTs) never
//! appear in plaintext in log files or terminal output, even if they
//! leak into a `tracing` event's message or fields.

use std::io::{self, Write};

/// A writer wrapper that redacts sensitive data from each output line.
///
/// Internally buffers bytes until a newline (`\n`) is encountered, then
/// redacts the complete line before writing it to the inner writer.
/// Any remaining buffered content is flushed (and redacted) on [`Write::flush`]
/// or on drop.
///
/// # Example
///
/// ```ignore
/// use gateway::runtime::RedactWriter;
/// let stdout = RedactWriter::new(std::io::stdout());
/// // Use with tracing-subscriber:
/// let layer = tracing_subscriber::fmt::layer().with_writer(stdout);
/// ```
pub struct RedactWriter<W: Write> {
    inner: Option<W>,
    buf: Vec<u8>,
}

impl<W: Write> RedactWriter<W> {
    /// Wrap an existing writer with sensitive-data redaction.
    pub fn new(inner: W) -> Self {
        Self {
            inner: Some(inner),
            buf: Vec::with_capacity(4096),
        }
    }

    /// Consume the wrapper and return the inner writer, flushing any
    /// buffered content first.
    pub fn into_inner(mut self) -> io::Result<W> {
        self.flush_inner()?;
        Ok(self.inner.take().expect("inner always Some before take"))
    }
}

// Internal helper so both flush() and write() can access inner without
// repeating the unwrap.
impl<W: Write> RedactWriter<W> {
    fn inner_mut(&mut self) -> &mut W {
        self.inner.as_mut().expect("inner always Some during use")
    }

    fn flush_inner(&mut self) -> io::Result<()> {
        if !self.buf.is_empty() {
            let line_str = String::from_utf8_lossy(&self.buf);
            let redacted = kernel::redactor::redact_sensitive_data(&line_str);
            let owned = redacted.into_owned();
            self.inner_mut().write_all(owned.as_bytes())?;
            self.buf.clear();
        }
        self.inner_mut().flush()
    }
}

impl<W: Write> Write for RedactWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let data_len = data.len();
        self.buf.extend_from_slice(data);

        // Process and emit complete lines
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line = &self.buf[..=pos]; // includes the newline
            let line_str = String::from_utf8_lossy(line);
            let redacted = kernel::redactor::redact_sensitive_data(&line_str);
            let owned = redacted.into_owned();
            self.inner_mut().write_all(owned.as_bytes())?;
            self.buf.drain(..=pos);
        }

        Ok(data_len)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_inner()
    }
}

impl<W: Write> Drop for RedactWriter<W> {
    fn drop(&mut self) {
        if let Some(ref mut inner) = self.inner
            && !self.buf.is_empty() {
                let line_str = String::from_utf8_lossy(&self.buf);
                let redacted = kernel::redactor::redact_sensitive_data(&line_str);
                let owned = redacted.into_owned();
                let _ = inner.write_all(owned.as_bytes());
                let _ = inner.flush();
                self.buf.clear();
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_api_key_in_output() {
        let mut buf = Vec::new();
        {
            let mut writer = RedactWriter::new(&mut buf);
            write!(
                writer,
                "INFO gateway: Loaded API key: sk-abc123def456ghi789jkl012mno345\n"
            )
            .unwrap();
            writer.flush().unwrap();
        }
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("REDACTED"), "got: {output}");
        assert!(!output.contains("sk-abc123def456"));
    }

    #[test]
    fn clean_output_passes_through() {
        let mut buf = Vec::new();
        {
            let mut writer = RedactWriter::new(&mut buf);
            write!(writer, "INFO gateway: starting up\n").unwrap();
            writer.flush().unwrap();
        }
        let output = String::from_utf8(buf).unwrap();
        assert_eq!(output, "INFO gateway: starting up\n");
    }

    #[test]
    fn handles_partial_line_on_flush() {
        let mut buf = Vec::new();
        {
            let mut writer = RedactWriter::new(&mut buf);
            // Write partial line without newline
            write!(writer, "partial line with sk-abc123def456ghi789jkl012mno345").unwrap();
            writer.flush().unwrap();
        }
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("REDACTED"), "got: {output}");
    }

    #[test]
    fn handles_multiple_lines() {
        let mut buf = Vec::new();
        {
            let mut writer = RedactWriter::new(&mut buf);
            write!(
                writer,
                "INFO start\nDEBUG api_key=sk-secret123456789\nWARN done\n"
            )
            .unwrap();
            writer.flush().unwrap();
        }
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("[REDACTED]"));
        assert!(output.contains("INFO start\n"));
        assert!(output.contains("WARN done\n"));
    }

    #[test]
    fn into_inner_works() {
        let mut inner = Vec::new();
        {
            let mut writer = RedactWriter::new(&mut inner);
            write!(writer, "token: sk-abc123def456ghi789jkl012mno\n").unwrap();
            let recovered = writer.into_inner().unwrap();
            // recovered is the original inner writer
            let _ = recovered;
        }
        let output = String::from_utf8(inner).unwrap();
        assert!(output.contains("REDACTED"), "got: {output}");
    }

    #[test]
    fn drop_flushes_remaining_content() {
        let mut inner = Vec::new();
        {
            let mut writer = RedactWriter::new(&mut inner);
            // Write partial line, then drop without explicit flush
            write!(writer, "Bearer abc123def456ghi789tokenjkl012mno345").unwrap();
            // writer dropped here — Drop should flush and redact
        }
        let output = String::from_utf8(inner).unwrap();
        assert!(output.contains("REDACTED"), "got: {output}");
        assert!(!output.contains("abc123def456"));
    }
}
