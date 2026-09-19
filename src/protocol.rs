use crate::Result;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
pub const VERSION: &str = "4.1";
pub const MAX_TEXT: usize = 65536;
pub const WIRE_MAX: usize = (1 << 18) - 5;
#[derive(Debug, PartialEq)]
pub enum Message {
    Text(Option<Vec<u8>>),
    Ack(u64),
}
pub fn valid(data: &[u8]) -> bool {
    data.len() <= MAX_TEXT && !data.contains(&0) && std::str::from_utf8(data).is_ok()
}
pub async fn read<R: AsyncRead + Unpin>(r: &mut R) -> Result<Message> {
    match r
        .read_u8()
        .await
        .map_err(|_| "Android control channel closed; reconnecting")?
    {
        0 => {
            let n = r
                .read_u32()
                .await
                .map_err(|_| "Truncated clipboard header")? as usize;
            if n > WIRE_MAX {
                return Err("Invalid clipboard frame length; check scrcpy compatibility");
            }
            if n > MAX_TEXT {
                // Drain rejected frames without allocating their declared length.
                let mut remaining = n;
                let mut chunk = [0; 4096];
                while remaining > 0 {
                    let count = remaining.min(chunk.len());
                    r.read_exact(&mut chunk[..count])
                        .await
                        .map_err(|_| "Truncated clipboard frame")?;
                    remaining -= count;
                }
                return Ok(Message::Text(None));
            }
            let mut data = vec![0; n];
            r.read_exact(&mut data)
                .await
                .map_err(|_| "Truncated clipboard frame")?;
            Ok(Message::Text(valid(&data).then_some(data)))
        }
        1 => Ok(Message::Ack(
            r.read_u64()
                .await
                .map_err(|_| "Truncated acknowledgement")?,
        )),
        _ => Err("Unexpected control message; incompatible server"),
    }
}
pub async fn write<W: AsyncWrite + Unpin>(w: &mut W, data: &[u8], seq: u64) -> Result<()> {
    if !valid(data) || seq == 0 {
        return Err("Invalid clipboard text or sequence");
    }
    let mut header = [0; 14];
    header[0] = 9;
    header[1..9].copy_from_slice(&seq.to_be_bytes());
    header[10..14].copy_from_slice(&(data.len() as u32).to_be_bytes());
    w.write_all(&header)
        .await
        .map_err(|_| "Android clipboard write failed")?;
    w.write_all(data)
        .await
        .map_err(|_| "Android clipboard write failed")
}
