use std::time::Duration;

use reqwest::{Client, StatusCode, Url, header};
use reviewq_app::access::AppError;

pub(crate) fn client(local: bool) -> Result<Client, AppError> {
    let mut builder = Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none());
    if local {
        builder = builder.no_proxy();
    }
    builder.build().map_err(|_| AppError::Internal)
}

pub(crate) async fn bounded_response(
    client: &Client,
    url: Url,
    headers: header::HeaderMap,
    max: usize,
) -> Result<(header::HeaderMap, Vec<u8>), AppError> {
    let mut response = client
        .get(url)
        .headers(headers)
        .send()
        .await
        .map_err(|_| AppError::Unavailable)?;
    match response.status() {
        StatusCode::OK => (),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => return Err(AppError::Forbidden),
        StatusCode::NOT_FOUND => return Err(AppError::NotFound),
        StatusCode::TOO_MANY_REQUESTS => return Err(AppError::Busy),
        _ => return Err(AppError::Unavailable),
    }
    let metadata = response.headers().clone();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| AppError::Unavailable)? {
        if chunk.len() > max - bytes.len() {
            return Err(AppError::PayloadTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok((metadata, bytes))
}
