//! Eight-byte prefix, checked before allocating header or payload buffers.
use crate::{MAX_HEADER, MAX_PAYLOAD, RecordHeader, ViewError};

pub struct Record {
    pub header: RecordHeader,
    pub payload: Vec<u8>,
}
impl Record {
    /// # Errors
    /// Returns a fixed protocol error for invalid, unsupported, or oversized data.
    pub fn encode(&self) -> Result<Vec<u8>, ViewError> {
        validate(&self.header, self.payload.len())?;
        let json = serde_json::to_vec(&self.header).map_err(|_| ViewError::InvalidRecord)?;
        if json.len() > MAX_HEADER {
            return Err(ViewError::LimitExceeded);
        }
        let mut bytes = Vec::with_capacity(8 + json.len() + self.payload.len());
        bytes.extend_from_slice(
            &u32::try_from(json.len())
                .map_err(|_| ViewError::LimitExceeded)?
                .to_be_bytes(),
        );
        bytes.extend_from_slice(
            &u32::try_from(self.payload.len())
                .map_err(|_| ViewError::LimitExceeded)?
                .to_be_bytes(),
        );
        bytes.extend_from_slice(&json);
        bytes.extend_from_slice(&self.payload);
        Ok(bytes)
    }
}
fn validate(header: &RecordHeader, payload: usize) -> Result<(), ViewError> {
    let version = match header {
        RecordHeader::Frame { v, .. }
        | RecordHeader::Status { v, .. }
        | RecordHeader::Heartbeat { v, .. } => *v,
    };
    if version != 1 {
        return Err(ViewError::UnsupportedVersion);
    }
    if payload > MAX_PAYLOAD {
        return Err(ViewError::LimitExceeded);
    }
    match header {
        RecordHeader::Frame { frame, .. }
            if payload == 0
                || frame.width == 0
                || frame.height == 0
                || frame.width > 1920
                || frame.height > 1080 =>
        {
            Err(ViewError::InvalidRecord)
        }
        RecordHeader::Status { .. } | RecordHeader::Heartbeat { .. } if payload != 0 => {
            Err(ViewError::InvalidRecord)
        }
        _ => Ok(()),
    }
}
/// Incremental parser; feed one bounded record at a time, even with fragmented HTTP chunks.
#[derive(Default)]
pub struct Decoder {
    prefix: [u8; 8],
    prefix_used: usize,
    lengths: Option<(usize, usize)>,
    buffer: Vec<u8>,
    failed: bool,
}
impl Decoder {
    /// Returns consumed bytes and at most one record. An error poisons this decoder.
    /// # Errors
    /// Returns a fixed protocol error for invalid, unsupported, or oversized data.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(usize, Option<Record>), ViewError> {
        if self.failed {
            return Err(ViewError::InvalidRecord);
        }
        let out = self.consume(bytes);
        if out.is_err() {
            self.failed = true;
            self.buffer.clear();
        }
        out
    }
    fn consume(&mut self, mut bytes: &[u8]) -> Result<(usize, Option<Record>), ViewError> {
        let original = bytes.len();
        if self.lengths.is_none() {
            let n = (8 - self.prefix_used).min(bytes.len());
            self.prefix[self.prefix_used..self.prefix_used + n].copy_from_slice(&bytes[..n]);
            self.prefix_used += n;
            bytes = &bytes[n..];
            if self.prefix_used < 8 {
                return Ok((original, None));
            }
            let h = u32::from_be_bytes(
                self.prefix[..4]
                    .try_into()
                    .map_err(|_| ViewError::InvalidRecord)?,
            ) as usize;
            let p = u32::from_be_bytes(
                self.prefix[4..]
                    .try_into()
                    .map_err(|_| ViewError::InvalidRecord)?,
            ) as usize;
            if h == 0 || h > MAX_HEADER || p > MAX_PAYLOAD {
                return Err(ViewError::LimitExceeded);
            }
            self.buffer = Vec::with_capacity(h + p);
            self.lengths = Some((h, p));
        }
        let (h, p) = self.lengths.ok_or(ViewError::InvalidRecord)?;
        let n = (h + p - self.buffer.len()).min(bytes.len());
        self.buffer.extend_from_slice(&bytes[..n]);
        let used = original - bytes.len() + n;
        if self.buffer.len() < h + p {
            return Ok((used, None));
        }
        let header =
            serde_json::from_slice(&self.buffer[..h]).map_err(|_| ViewError::InvalidRecord)?;
        validate(&header, p)?;
        let payload = self.buffer.split_off(h);
        self.buffer.clear();
        self.lengths = None;
        self.prefix_used = 0;
        Ok((used, Some(Record { header, payload })))
    }
    /// # Errors
    /// Returns a fixed protocol error for invalid, unsupported, or oversized data.
    pub fn finish(&self) -> Result<(), ViewError> {
        if self.failed || self.prefix_used != 0 {
            Err(ViewError::InvalidRecord)
        } else {
            Ok(())
        }
    }
}
