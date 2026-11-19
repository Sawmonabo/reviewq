use std::fs::File;
use std::io::{self, BufReader, Cursor, Read, Seek, SeekFrom};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use reviewq_app::access::AppError;
use tokio::io::AsyncWriteExt;

use super::{BODY_LIMIT, Response};

pub(crate) type ResponseReader = Box<dyn Read + Send>;

impl Response {
    /// Provides a bounded synchronous decoder input, spilling large bodies to a private file.
    pub(crate) async fn bounded_reader(
        mut self,
        profile: &Path,
        memory_limit: usize,
        max_bytes: usize,
    ) -> Result<ResponseReader, AppError> {
        let max_bytes = max_bytes.min(BODY_LIMIT);
        if self
            .content_length()
            .is_some_and(|size| size > max_bytes as u64)
        {
            return Err(AppError::PayloadTooLarge);
        }
        let mut prefix = Vec::new();
        let mut spool = None::<tokio::fs::File>;
        let mut length = 0;
        while let Some(chunk) = self.chunk().await? {
            if chunk.len() > max_bytes - length {
                return Err(AppError::PayloadTooLarge);
            }
            length += chunk.len();
            if spool.is_none() && length > memory_limit {
                let path = profile.to_owned();
                let file = tokio::task::spawn_blocking(move || private_file(&path))
                    .await
                    .map_err(|_| AppError::Internal)?
                    .map_err(|_| AppError::Internal)?;
                let mut file = tokio::fs::File::from_std(file);
                file.write_all(&prefix)
                    .await
                    .map_err(|_| AppError::Internal)?;
                prefix = Vec::new();
                spool = Some(file);
            }
            if let Some(file) = &mut spool {
                file.write_all(&chunk)
                    .await
                    .map_err(|_| AppError::Internal)?;
            } else {
                prefix.extend_from_slice(&chunk);
            }
        }
        match spool {
            None => Ok(Box::new(Cursor::new(prefix))),
            Some(mut file) => {
                file.flush().await.map_err(|_| AppError::Internal)?;
                let mut file = file.into_std().await;
                file.seek(SeekFrom::Start(0))
                    .map_err(|_| AppError::Internal)?;
                Ok(Box::new(BufReader::new(file)))
            }
        }
    }
}

pub(crate) fn private_file(profile: &Path) -> io::Result<File> {
    let file = tempfile::tempfile_in(profile)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(file)
}
