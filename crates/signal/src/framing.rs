//! LAN TCP framing: u32 big-endian length prefix + envelope JSON bytes.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Largest accepted frame (SDP + candidates fit in a few KiB; 1 MiB is generous).
pub const MAX_FRAME: usize = 1 << 20;

pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "signaling frame too large"));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, data: &[u8]) -> std::io::Result<()> {
    if data.len() > MAX_FRAME {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "signaling frame too large"));
    }
    let mut buf = Vec::with_capacity(4 + data.len());
    buf.extend_from_slice(&(data.len() as u32).to_be_bytes());
    buf.extend_from_slice(data);
    w.write_all(&buf).await?;
    w.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn roundtrip() {
        let (mut a, mut b) = tokio::io::duplex(64);
        let w = tokio::spawn(async move {
            write_frame(&mut a, b"hello").await.unwrap();
            write_frame(&mut a, &[7u8; 100]).await.unwrap();
        });
        assert_eq!(read_frame(&mut b).await.unwrap(), b"hello");
        assert_eq!(read_frame(&mut b).await.unwrap(), vec![7u8; 100]);
        w.await.unwrap();
    }
}
