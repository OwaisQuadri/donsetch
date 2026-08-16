//! HTTP/2 frame layer (RFC 7540 §4-6). Minimal client subset.

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::error::FetchError;

pub const DATA: u8 = 0x0;
pub const HEADERS: u8 = 0x1;
pub const PRIORITY: u8 = 0x2;
pub const RST_STREAM: u8 = 0x3;
pub const SETTINGS: u8 = 0x4;
pub const PUSH_PROMISE: u8 = 0x5;
pub const PING: u8 = 0x6;
pub const GOAWAY: u8 = 0x7;
pub const WINDOW_UPDATE: u8 = 0x8;
pub const CONTINUATION: u8 = 0x9;

pub const FLAG_END_STREAM: u8 = 0x1;
pub const FLAG_ACK: u8 = 0x1;
pub const FLAG_END_HEADERS: u8 = 0x4;
pub const FLAG_PADDED: u8 = 0x8;
pub const FLAG_PRIORITY: u8 = 0x20;

#[derive(Debug, Clone, Copy)]
pub struct FrameHeader {
    pub ty: u8,
    pub flags: u8,
    pub stream_id: u32,
}

pub async fn read_frame<R: AsyncReadExt + Unpin>(
    r: &mut R,
) -> Result<(FrameHeader, Vec<u8>), FetchError> {
    let mut hdr = [0u8; 9];
    r.read_exact(&mut hdr).await?;
    let len = u32::from_be_bytes([0, hdr[0], hdr[1], hdr[2]]);
    // 1 MiB cap: 16 MiB frames are accepted pre-SETTINGS today,
    // which is a cheap per-connection memory amplifier. Real
    // servers send SETTINGS_MAX_FRAME_SIZE ≤ 16 KiB and never
    // need anything close to 1 MiB.
    if len > 1 << 20 {
        return Err(FetchError::Http(format!("h2 frame too large: {len}")));
    }
    let header = FrameHeader {
        ty: hdr[3],
        flags: hdr[4],
        stream_id: u32::from_be_bytes([hdr[5] & 0x7f, hdr[6], hdr[7], hdr[8]]),
    };
    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload).await?;
    Ok((header, payload))
}

pub async fn write_frame<W: AsyncWriteExt + Unpin>(
    w: &mut W,
    ty: u8,
    flags: u8,
    stream_id: u32,
    payload: &[u8],
) -> Result<(), FetchError> {
    let len = payload.len() as u32;
    let hdr = [
        (len >> 16) as u8,
        (len >> 8) as u8,
        len as u8,
        ty,
        flags,
        ((stream_id >> 24) & 0x7f) as u8,
        (stream_id >> 16) as u8,
        (stream_id >> 8) as u8,
        stream_id as u8,
    ];
    w.write_all(&hdr).await?;
    w.write_all(payload).await?;
    Ok(())
}

pub fn settings_payload(pairs: &[(u16, u32)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pairs.len() * 6);
    for (id, val) in pairs {
        out.extend_from_slice(&id.to_be_bytes());
        out.extend_from_slice(&val.to_be_bytes());
    }
    out
}
