//! Admission limits for paginated tools/list responses.

use std::{collections::HashSet, io, time::Duration};

use rmcp::model::{ListToolsResult, PaginatedRequestParams, Tool};

use crate::{McpError, Result, session::Session};

const MAX_PAGES: usize = 32;
const MAX_TOOLS: usize = 1024;
const MAX_SCHEMA_BYTES: usize = 4 * 1024 * 1024;
const MAX_CURSOR_BYTES: usize = 4096;

#[derive(Default)]
struct Budget {
    pages: usize,
    tools: usize,
    bytes: usize,
    cursors: HashSet<String>,
}

impl Budget {
    fn accept(&mut self, page: &ListToolsResult) -> std::result::Result<(), &'static str> {
        self.pages += 1;
        self.tools = self.tools.saturating_add(page.tools.len());
        if self.tools > MAX_TOOLS {
            return Err("tools/list exceeds the tool-count limit");
        }
        let mut count = ByteBudget(MAX_SCHEMA_BYTES - self.bytes);
        for tool in &page.tools {
            // Count the serialized schema plus metadata, including descriptions;
            // count without allocating another copy of untrusted JSON.
            if serde_json::to_writer(&mut count, tool).is_err() {
                return Err("tools/list exceeds the schema/metadata byte limit");
            }
        }
        self.bytes = MAX_SCHEMA_BYTES - count.0;
        if let Some(cursor) = &page.next_cursor {
            if self.pages >= MAX_PAGES {
                return Err("tools/list exceeds the page limit");
            }
            if cursor.len() > MAX_CURSOR_BYTES {
                return Err("tools/list exceeds the cursor byte limit");
            }
            if !self.cursors.insert(cursor.clone()) {
                return Err("tools/list repeated a pagination cursor");
            }
        }
        Ok(())
    }
}

struct ByteBudget(usize);

impl io::Write for ByteBudget {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_sub(buf.len())
            .ok_or_else(|| io::Error::other("byte limit"))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// One deadline covers every page; accepted pages never exceed the aggregate
/// tool/schema limits. The caller publishes only a complete, successful list.
pub(crate) async fn discover(
    session: &Session,
    server: &str,
    timeout: Duration,
) -> Result<Vec<Tool>> {
    let list = async {
        let mut budget = Budget::default();
        let mut cursor = None;
        let mut tools = Vec::new();
        loop {
            let page = session
                .peer()
                .list_tools(Some(PaginatedRequestParams::default().with_cursor(cursor)))
                .await
                .map_err(|e| McpError::Transport {
                    server: server.to_owned(),
                    message: format!("tools/list: {e}"),
                })?;
            budget
                .accept(&page)
                .map_err(|message| McpError::Transport {
                    server: server.to_owned(),
                    message: message.to_owned(),
                })?;
            tools.extend(page.tools);
            cursor = page.next_cursor;
            if cursor.is_none() {
                return Ok(tools);
            }
        }
    };
    tokio::time::timeout(timeout, list)
        .await
        .map_err(|_| McpError::Timeout {
            server: server.to_owned(),
            seconds: timeout.as_secs().max(1),
        })?
        .map_err(|error| session.redactor.error(error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn tool() -> Tool {
        Tool::new("test", "test", Arc::default())
    }

    fn page(cursor: Option<String>, tools: Vec<Tool>) -> ListToolsResult {
        let mut page = ListToolsResult::with_all_items(tools);
        page.next_cursor = cursor;
        page
    }

    #[test]
    fn caps_pages_tools_schema_bytes_and_cursors() {
        let mut budget = Budget::default();
        for n in 1..MAX_PAGES {
            budget.accept(&page(Some(n.to_string()), vec![])).unwrap();
        }
        assert!(
            budget
                .accept(&page(Some("last".into()), vec![]))
                .unwrap_err()
                .contains("page limit")
        );

        let mut budget = Budget::default();
        budget
            .accept(&page(Some("next".into()), vec![tool(); MAX_TOOLS]))
            .unwrap();
        assert!(
            budget
                .accept(&page(None, vec![tool()]))
                .unwrap_err()
                .contains("tool-count")
        );

        let mut budget = Budget::default();
        let mut large = tool();
        large.input_schema = Arc::new(
            serde_json::from_value(
                serde_json::json!({"description": "x".repeat(MAX_SCHEMA_BYTES / 2)}),
            )
            .unwrap(),
        );
        budget
            .accept(&page(Some("next".into()), vec![large.clone()]))
            .unwrap();
        assert!(
            budget
                .accept(&page(None, vec![large]))
                .unwrap_err()
                .contains("byte limit")
        );

        let mut budget = Budget::default();
        let repeated = page(Some("same".into()), vec![]);
        budget.accept(&repeated).unwrap();
        assert!(budget.accept(&repeated).unwrap_err().contains("repeated"));
        assert!(
            Budget::default()
                .accept(&page(Some("x".repeat(MAX_CURSOR_BYTES + 1)), vec![]))
                .unwrap_err()
                .contains("cursor byte")
        );
    }

    #[test]
    fn exact_final_page_and_tool_limits_are_accepted() {
        let mut budget = Budget::default();
        for n in 1..MAX_PAGES {
            budget.accept(&page(Some(n.to_string()), vec![])).unwrap();
        }
        budget.accept(&page(None, vec![tool(); MAX_TOOLS])).unwrap();
    }
}
