use std::io::{self, Cursor, Read};

use reviewq_app::access::AppError;
use tokio::sync::mpsc;

use super::{Response, ResponseReader};

const DECODE_CHUNK_BYTES: usize = 32 * 1024;

impl Response {
    /// Feeds a synchronous decoder directly, with one queued chunk and no response spool.
    pub(crate) async fn decode<T: Send + 'static>(
        mut self,
        decode: impl FnOnce(ResponseReader) -> Result<T, AppError> + Send + 'static,
    ) -> Result<T, AppError> {
        if self
            .content_length()
            .is_some_and(|size| size > super::BODY_LIMIT as u64)
        {
            return Err(AppError::PayloadTooLarge);
        }
        let (sender, receiver) = mpsc::channel(1);
        let pump = async move {
            while let Some(chunk) = self.chunk().await? {
                for part in chunk.chunks(DECODE_CHUNK_BYTES) {
                    let Ok(permit) = sender.reserve().await else {
                        return Ok(());
                    };
                    permit.send(part.to_vec());
                }
            }
            Ok(())
        };
        let worker = tokio::task::spawn_blocking(move || {
            decode(Box::new(Chunks {
                current: Cursor::new(Vec::new()),
                receiver,
            }))
        });
        let (_, decoded) = tokio::try_join!(pump, async {
            worker.await.map_err(|_| AppError::Unavailable)?
        })?;
        Ok(decoded)
    }
}

struct Chunks {
    current: Cursor<Vec<u8>>,
    receiver: mpsc::Receiver<Vec<u8>>,
}

impl Read for Chunks {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let read = self.current.read(bytes)?;
        if read != 0 {
            return Ok(read);
        }
        let Some(next) = self.receiver.blocking_recv() else {
            return Ok(0);
        };
        self.current = Cursor::new(next);
        self.current.read(bytes)
    }
}
