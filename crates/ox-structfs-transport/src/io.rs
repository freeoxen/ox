use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::{CodecError, WireCodec, WireMessage};

pub(crate) async fn read_message<R: AsyncRead + Unpin>(
    reader: &mut R,
    codec: &WireCodec,
) -> Result<Option<WireMessage>, StreamError> {
    let mut prefix = [0_u8; 4];
    match reader.read_exact(&mut prefix[..1]).await {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(StreamError::Io(error)),
    }
    reader.read_exact(&mut prefix[1..]).await?;
    let payload_len = u32::from_be_bytes(prefix) as usize;
    if payload_len > codec.limits().max_frame_bytes {
        return Err(StreamError::Codec(CodecError::FrameTooLarge {
            actual: payload_len,
            limit: codec.limits().max_frame_bytes,
        }));
    }

    let frame_len = 4_usize
        .checked_add(payload_len)
        .ok_or(StreamError::Codec(CodecError::LengthOverflow))?;
    let mut frame = Vec::with_capacity(frame_len);
    frame.extend_from_slice(&prefix);
    frame.resize(frame_len, 0);
    reader.read_exact(&mut frame[4..]).await?;
    codec.decode(&frame).map(Some).map_err(StreamError::Codec)
}

pub(crate) async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &[u8],
) -> io::Result<()> {
    writer.write_all(frame).await?;
    writer.flush().await
}

#[derive(Debug)]
pub(crate) enum StreamError {
    Io(io::Error),
    Codec(CodecError),
}

impl From<io::Error> for StreamError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<CodecError> for StreamError {
    fn from(error: CodecError) -> Self {
        Self::Codec(error)
    }
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Codec(error) => write!(formatter, "{error}"),
        }
    }
}
