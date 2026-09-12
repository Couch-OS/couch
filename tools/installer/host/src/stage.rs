//! Bounded native wire framing for an already authenticated Linux-stage stream.
//! This module never opens a connection or authorizes storage writes. Callers must
//! bind the stage certificate/token/plan over the selected USB session first.
use anyhow::{ensure, Context, Result};
use flate2::{Decompress, FlushDecompress, Status};
use serde_json::Value;
use std::io::{Read, Write};
pub const CHUNK: usize = 1024 * 1024;

pub struct Channel<S> {
    stream: S,
}
impl<S: Read + Write> Channel<S> {
    pub fn authenticated(stream: S) -> Self {
        Self { stream }
    }
    #[cfg(test)]
    pub(crate) fn into_inner(self) -> S {
        self.stream
    }
    pub fn begin_install(&mut self, plan: &Value) -> Result<()> {
        ensure!(plan.is_object(), "expected install plan");
        let bytes = zeroize::Zeroizing::new(serde_json::to_vec(plan)?);
        ensure!(
            !bytes.is_empty() && bytes.len() <= CHUNK,
            "install plan exceeds frame bound"
        );
        self.stream.write_all(b"CBP1")?;
        self.stream.write_all(&10u32.to_le_bytes())?;
        self.stream.write_all(&0u64.to_le_bytes())?;
        self.stream.write_all(&(bytes.len() as u32).to_le_bytes())?;
        self.stream.write_all(&bytes)?;
        self.stream.flush()?;
        Ok(())
    }
    pub fn send_json(&mut self, value: &Value) -> Result<()> {
        ensure!(value.is_object(), "expected JSON object");
        let bytes = zeroize::Zeroizing::new(serde_json::to_vec(value)?);
        ensure!(
            !bytes.is_empty() && bytes.len() <= CHUNK,
            "JSON frame exceeds bound"
        );
        self.stream.write_all(&(bytes.len() as u32).to_le_bytes())?;
        self.stream.write_all(&bytes)?;
        self.stream.flush()?;
        Ok(())
    }
    pub fn receive_json(&mut self) -> Result<Value> {
        let mut header = [0; 4];
        self.stream
            .read_exact(&mut header)
            .context("stage JSON header interrupted")?;
        let size = u32::from_le_bytes(header) as usize;
        ensure!(size > 0 && size <= CHUNK, "invalid JSON frame size");
        let mut bytes = zeroize::Zeroizing::new(vec![0; size]);
        self.stream
            .read_exact(&mut bytes)
            .context("stage JSON body interrupted")?;
        let value: Value = serde_json::from_slice(&bytes).context("invalid stage JSON")?;
        ensure!(value.is_object(), "expected stage object");
        ensure!(
            value["event"] != "error",
            "device stopped installation; preserve originals and journal"
        );
        Ok(value)
    }
    /// Raw image chunks are valid on all protocol versions. Compression is an
    /// optional throughput optimization; no host compression subprocess is used.
    pub fn send_chunk(&mut self, bytes: &[u8]) -> Result<()> {
        ensure!(
            !bytes.is_empty() && bytes.len() <= CHUNK,
            "invalid image chunk size"
        );
        let size = (bytes.len() as u32).to_le_bytes();
        self.stream.write_all(&size)?;
        self.stream.write_all(&size)?;
        self.stream.write_all(&0u32.to_le_bytes())?;
        self.stream.write_all(bytes)?;
        self.stream.flush()?;
        Ok(())
    }
    pub fn receive_chunk(&mut self, expected: usize) -> Result<Vec<u8>> {
        ensure!(
            expected > 0 && expected <= CHUNK,
            "invalid expected backup size"
        );
        let mut header = [0; 12];
        self.stream
            .read_exact(&mut header)
            .context("stage backup header interrupted")?;
        let size = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
        let wire = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
        let kind = u32::from_le_bytes(header[8..].try_into().unwrap());
        ensure!(
            size == expected && wire > 0 && wire <= CHUNK && kind <= 1,
            "invalid backup frame"
        );
        let mut input = vec![0; wire];
        self.stream
            .read_exact(&mut input)
            .context("stage backup body interrupted")?;
        if kind == 0 {
            ensure!(wire == size, "raw backup length mismatch");
            return Ok(input);
        }
        let mut decoder = Decompress::new(true);
        let mut output = vec![0; size + 1];
        let status = decoder.decompress(&input, &mut output, FlushDecompress::Finish)?;
        ensure!(
            status == Status::StreamEnd
                && decoder.total_in() == wire as u64
                && decoder.total_out() == size as u64,
            "compressed backup truncated, oversized or trailing"
        );
        output.truncate(size);
        Ok(output)
    }
    pub fn expect(&mut self, expected: &Value) -> Result<()> {
        ensure!(
            self.receive_json()? == *expected,
            "unexpected stage transition"
        );
        Ok(())
    }
    pub fn acknowledge(&mut self, event: &str, target: &str, sha256: &str) -> Result<()> {
        self.send_json(&serde_json::json!({"ack":event,"target":target,"sha256":sha256}))
    }
    /// Progress never grants an acknowledgement: caller must check the returned
    /// final phase/hash against its durable journal and independently saved data.
    pub fn verification(
        &mut self,
        target: &str,
        phase: &str,
        total: u64,
        mut progress: impl FnMut(u64, u64) -> Result<()>,
    ) -> Result<Value> {
        ensure!(total > 0, "invalid verification size");
        let mut previous = None;
        for _ in 0..total.div_ceil(CHUNK as u64).saturating_add(3) {
            let event = self.receive_json()?;
            if event["event"] != "verify_progress" {
                ensure!(
                    previous.is_none() || previous == Some(total),
                    "verification ended early"
                );
                return Ok(event);
            }
            let done = event["done"].as_u64().context("invalid progress count")?;
            ensure!(
                event.as_object().unwrap().len() == 5
                    && event["target"] == target
                    && event["phase"] == phase
                    && event["total"].as_u64() == Some(total)
                    && done <= total
                    && previous.map_or(done == 0, |last| done > last),
                "invalid verification progress"
            );
            previous = Some(done);
            progress(done, total)?;
        }
        anyhow::bail!("excessive verification progress")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    fn json_frames(events: &[Value]) -> Cursor<Vec<u8>> {
        let mut bytes = Vec::new();
        for event in events {
            let data = serde_json::to_vec(event).unwrap();
            bytes.extend((data.len() as u32).to_le_bytes());
            bytes.extend(data);
        }
        Cursor::new(bytes)
    }
    fn progress(done: u64) -> Value {
        serde_json::json!({"event":"verify_progress","target":"userdata","phase":"write","done":done,"total":4096})
    }
    #[test]
    fn verification_progress_must_finish_before_final_event() {
        let final_event = serde_json::json!({"event":"verified","target":"userdata"});
        let mut counts = Vec::new();
        let mut channel = Channel::authenticated(json_frames(&[
            progress(0),
            progress(4096),
            final_event.clone(),
        ]));
        assert_eq!(
            channel
                .verification("userdata", "write", 4096, |done, _| {
                    counts.push(done);
                    Ok(())
                })
                .unwrap(),
            final_event
        );
        assert_eq!(counts, [0, 4096]);
        for events in [
            vec![progress(1)],
            vec![progress(0), progress(0)],
            vec![progress(0), final_event],
        ] {
            assert!(Channel::authenticated(json_frames(&events))
                .verification("userdata", "write", 4096, |_, _| Ok(()))
                .is_err());
        }
    }
    #[test]
    fn malformed_lengths_and_interrupted_frames_fail_closed() {
        for size in [0, CHUNK as u32 + 1] {
            assert!(Channel::authenticated(Cursor::new(size.to_le_bytes()))
                .receive_json()
                .is_err());
        }
        assert!(Channel::authenticated(Cursor::new(100u32.to_le_bytes()))
            .receive_json()
            .is_err());
        assert!(
            Channel::authenticated(json_frames(&[serde_json::json!({"event":"error"})]))
                .receive_json()
                .is_err()
        );
    }
    #[test]
    fn compressed_backup_rejects_trailing_truncated_and_overexpanded_bytes() {
        use flate2::{write::ZlibEncoder, Compression};
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(&[42; 4096]).unwrap();
        let good = encoder.finish().unwrap();
        for (bytes, expected, ok) in [
            (good.clone(), 4096, true),
            (good[..good.len() - 1].to_vec(), 4096, false),
            ([good.clone(), vec![0]].concat(), 4096, false),
            (good, 4095, false),
        ] {
            let mut wire = Vec::new();
            wire.extend((expected as u32).to_le_bytes());
            wire.extend((bytes.len() as u32).to_le_bytes());
            wire.extend(1u32.to_le_bytes());
            wire.extend(bytes);
            let result = Channel::authenticated(Cursor::new(wire)).receive_chunk(expected);
            assert_eq!(result.is_ok(), ok);
            if ok {
                assert_eq!(result.unwrap(), vec![42; 4096]);
            }
        }
    }
}
