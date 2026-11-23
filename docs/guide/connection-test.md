# Test connection

Every key and app connection in Praxi-Bot has a **Test connection** button: AI provider keys
(Settings → AI), web search and dictation keys (Settings), e-mail accounts, MCP servers (Apps) and
the first-run key step. The test runs on the Praxi service on your computer, never shows your key,
gives up after about 12 seconds and answers with one of the results below. The HTTP status and the
service's own error code are in the **Details** box.

An MCP server test starts the server and lists its tools. It never runs a tool.

<a id="bad-key"></a>
## Bad key

The service answered "no" (HTTP 401/403, or a message such as "API key not valid"). Copy the key again
from the provider's website, check that nothing is missing at the start or end and that it belongs to
the right provider, then save it again. A key you pasted into a chat or a note may have been revoked
by the provider; create a new one.

<a id="no-network"></a>
## No network

The service could not be reached, or did not answer in time. Check your internet connection, VPN and
proxy. For a program on this computer (Ollama, LM Studio, an MCP server) make sure it is running.

<a id="quota-exceeded"></a>
## Quota exceeded

The key is fine, but the plan, credit or rate limit is used up (HTTP 402/429). Wait a little and try
again, or check your plan and credit on the provider's website. See also "Rate limit errors from Gemini"
in [Troubleshooting](troubleshooting.md).

<a id="wrong-endpoint"></a>
## Wrong endpoint

The server answered, but not like this service (HTTP 404, or a page that is not an API), or an MCP
server's command could not be started. Check the web address you entered, including `https://` and the
path (many services want a path ending in `/v1`), or the command of the MCP server.

<a id="provider-outage"></a>
## Provider outage

The service itself has a problem (HTTP 5xx). Try again in a few minutes, or look at the provider's status page.

<a id="unknown"></a>
## Unknown result

None of the above. Open **Details**, compare the status and code with the provider's documentation, and
copy the details when you ask for help. They never contain your key.
