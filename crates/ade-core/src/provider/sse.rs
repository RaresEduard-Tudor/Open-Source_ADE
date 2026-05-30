//! Minimal Server-Sent Events reader for streaming responses.
//!
//! Reads the response body in chunks, splits on newlines, and yields the
//! payload of each `data:` line. `[DONE]` (OpenAI) and natural EOF both end the
//! stream; `event:` and blank lines are ignored. Bytes are buffered until a full
//! line arrives, so multibyte characters never split across chunk boundaries.

use crate::error::{Error, Result};

/// Call `f` once per `data:` payload until the stream ends.
pub async fn for_each_data<F>(mut resp: reqwest::Response, mut f: F) -> Result<()>
where
    F: FnMut(&str) -> Result<()> + Send,
{
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let chunk = resp
            .chunk()
            .await
            .map_err(|e| Error::Provider(format!("stream read: {e}")))?;
        let Some(chunk) = chunk else { break };
        buf.extend_from_slice(&chunk);

        while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buf.drain(..=nl).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end();
            if let Some(data) = line.strip_prefix("data:") {
                let data = data.trim();
                if data == "[DONE]" {
                    return Ok(());
                }
                if !data.is_empty() {
                    f(data)?;
                }
            }
        }
    }
    Ok(())
}
