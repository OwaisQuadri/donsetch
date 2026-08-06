//! Streaming body decompression: br / gzip / deflate / zstd.

use std::io::Read;

use crate::error::FetchError;

pub fn decompress(encoding: &str, body: &[u8]) -> Result<Vec<u8>, FetchError> {
    match encoding.trim().to_ascii_lowercase().as_str() {
        "" | "identity" => Ok(body.to_vec()),
        "br" => {
            let mut out = Vec::new();
            brotli::Decompressor::new(body, 1 << 20)
                .read_to_end(&mut out)
                .map_err(|e| FetchError::Http(format!("brotli: {e}")))?;
            Ok(out)
        }
        "gzip" => {
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(body)
                .read_to_end(&mut out)
                .map_err(|e| FetchError::Http(format!("gzip: {e}")))?;
            Ok(out)
        }
        "deflate" => {
            let mut out = Vec::new();
            flate2::read::ZlibDecoder::new(body)
                .read_to_end(&mut out)
                .map_err(|e| FetchError::Http(format!("deflate: {e}")))?;
            Ok(out)
        }
        "zstd" => zstd::decode_all(body).map_err(|e| FetchError::Http(format!("zstd: {e}"))),
        other => Err(FetchError::Http(format!(
            "unknown content-encoding: {other}"
        ))),
    }
}
