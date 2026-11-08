//! Byte budgets for data controlled by provider endpoints. Limits are checked
//! before extending buffers; an HTTP Content-Length is only a fast path.

use futures::{Stream, StreamExt, pin_mut};
use serde_json::Value;

use crate::{error::ProviderError, util::map_reqwest_error};

pub(crate) const MAX_SSE_LINE_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_SSE_EVENT_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_SSE_TOTAL_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const MAX_TOOL_ARGUMENT_BYTES: usize = 1024 * 1024;
const MAX_TOTAL_TOOL_ARGUMENT_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_ERROR_BYTES: usize = 64 * 1024;

pub(crate) fn checked_size(
    used: usize,
    additional: usize,
    limit: usize,
    kind: &str,
) -> Result<usize, ProviderError> {
    used.checked_add(additional)
        .filter(|&size| size <= limit)
        .ok_or_else(|| ProviderError::Stream(format!("{kind} exceeds {limit}-byte limit")))
}

/// Bound both each assembled call and all argument strings retained in a turn.
#[derive(Debug, Default)]
pub(crate) struct ToolArgumentBudget {
    total: usize,
}

impl ToolArgumentBudget {
    pub(crate) fn charge(
        &mut self,
        current: usize,
        additional: usize,
    ) -> Result<(), ProviderError> {
        checked_size(
            current,
            additional,
            MAX_TOOL_ARGUMENT_BYTES,
            "tool arguments",
        )?;
        self.total = checked_size(
            self.total,
            additional,
            MAX_TOTAL_TOOL_ARGUMENT_BYTES,
            "total tool arguments",
        )?;
        Ok(())
    }
}

async fn read_body(
    response: reqwest::Response,
    limit: usize,
    truncate: bool,
) -> Result<Vec<u8>, ProviderError> {
    if !truncate
        && response
            .content_length()
            .is_some_and(|len| len > limit as u64)
    {
        return Err(ProviderError::Stream(format!(
            "response exceeds {limit}-byte limit"
        )));
    }
    let chunks = response
        .bytes_stream()
        .map(|result| result.map_err(map_reqwest_error));
    read_chunks(chunks, limit, truncate).await
}

async fn read_chunks<S, B>(
    chunks: S,
    limit: usize,
    truncate: bool,
) -> Result<Vec<u8>, ProviderError>
where
    S: Stream<Item = Result<B, ProviderError>>,
    B: AsRef<[u8]>,
{
    pin_mut!(chunks);
    let mut body = Vec::new();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk?;
        let chunk = chunk.as_ref();
        let remaining = limit - body.len();
        if truncate && chunk.len() >= remaining {
            body.extend_from_slice(&chunk[..remaining]);
            // Drop the response now, including a held-open/chunked error body.
            break;
        }
        checked_size(body.len(), chunk.len(), limit, "response")?;
        body.extend_from_slice(chunk);
    }
    Ok(body)
}

/// Preserve status-based error/retry classification, but never buffer more than
/// a small prefix of the error body. The existing classifiers shorten it further.
pub(crate) async fn error_body(response: reqwest::Response) -> Result<String, ProviderError> {
    let body = read_body(response, MAX_ERROR_BYTES, true).await?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

pub(crate) async fn response_text(response: reqwest::Response) -> Result<String, ProviderError> {
    let body = read_body(response, MAX_RESPONSE_BYTES, false).await?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

pub(crate) async fn response_json(response: reqwest::Response) -> Result<Value, ProviderError> {
    let body = read_body(response, MAX_RESPONSE_BYTES, false).await?;
    serde_json::from_slice(&body)
        .map_err(|error| ProviderError::Stream(format!("invalid JSON response: {error}")))
}

#[cfg(test)]
mod tests {
    use futures::{FutureExt, stream};

    use super::*;

    #[test]
    fn oversized_error_body_is_truncated_without_reading_tail() {
        let chunks = stream::iter([
            Ok(vec![b'x'; MAX_ERROR_BYTES / 2]),
            Ok(vec![b'y'; MAX_ERROR_BYTES]),
        ])
        .chain(stream::pending());
        let body = read_chunks(chunks, MAX_ERROR_BYTES, true)
            .now_or_never()
            .expect("the error prefix must not wait for EOF")
            .unwrap();
        assert_eq!(body.len(), MAX_ERROR_BYTES);
        assert!(body[..MAX_ERROR_BYTES / 2].iter().all(|&byte| byte == b'x'));
        assert!(body[MAX_ERROR_BYTES / 2..].iter().all(|&byte| byte == b'y'));
    }

    #[test]
    fn chunked_success_body_errors_before_reading_tail() {
        let chunks = stream::iter([Ok(vec![b'x'; 4]), Ok(vec![b'y'; 5])]).chain(stream::pending());
        let result = read_chunks(chunks, 8, false)
            .now_or_never()
            .expect("overflow must not wait for EOF");
        assert!(
            matches!(result, Err(ProviderError::Stream(message)) if message.contains("8-byte"))
        );
    }

    #[test]
    fn exact_body_limit_is_allowed_and_split_utf8_is_preserved() {
        let utf8 = "Masaüstü".as_bytes();
        let chunks = stream::iter([Ok(utf8[..5].to_vec()), Ok(utf8[5..].to_vec())]);
        let body = read_chunks(chunks, utf8.len(), false)
            .now_or_never()
            .unwrap()
            .unwrap();
        assert_eq!(body, utf8);
    }

    #[test]
    fn tool_argument_budget_bounds_each_call_and_the_total() {
        let mut budget = ToolArgumentBudget::default();
        assert!(budget.charge(MAX_TOOL_ARGUMENT_BYTES, 1).is_err());
        // A failed charge does not poison the remaining budget.
        for _ in 0..4 {
            budget.charge(0, MAX_TOOL_ARGUMENT_BYTES).unwrap();
        }
        assert!(budget.charge(0, 1).is_err());
    }
}
