# Rust network tools: `web_search` and `web_reader` (#30)

This is an opt-in read-only network slice of the existing request-scoped `LLM → Tool → LLM` continuation. It applies to structured v3 `tool_call` and the native OpenAI/Gemini function-call paths. No new dependencies, Python workers, WebView, browser automation, or generic scripting interface are introduced.

## Configuration and authorization

`AppSettings.tools` (`SettingsApi::edit::<ToolSettings>`) owns a nested `web: WebToolSettings`:

| Setting | Default | Meaning |
|---|---|---|
| `tools.enabled` | `false` | Global tool execution permission |
| `tools.max_depth` | `2` | Maximum number of tool-continuation turns; range 0–4 |
| `tools.web.enabled` | `false` | Independent opt-in for all network tools |
| `tools.web.search_enabled` | `false` | Allow `web_search` |
| `tools.web.search_endpoint` | `null` | Explicitly configured HTTPS SearXNG-compatible `/search` endpoint; no embedded query or credentials |
| `tools.web.reader_enabled` | `false` | Allow `web_reader` |
| `tools.web.reader_allowed_hosts` | `[]` | Explicit exact-match DNS hostname allowlist; no wildcards; max 32 hosts |

All gates must be enabled for the matching tool, and the search endpoint / reader allowlist must be present. Settings are validated and **captured at request admission**; later edits cannot expand permissions of an active request. A web-search result URL does **not** confer permission to fetch it with `web_reader`: the target must independently match the reader allowlist.

A search endpoint is an operator-chosen trusted API, *not* an arbitrary URL passed by the model. This slice supports the SearXNG-style JSON GET contract `?q=<encoded>&format=json` with `results[].{title,url,content}` and does not claim that every SearXNG instance enables its JSON format or that every search provider implements this contract. API keys and Google Search are not implemented. If no endpoint is configured, the LLM is **not advertised** `web_search`.

## Request and output contract

- `web_search({"query":"...","limit":5})`: query 1–256 bytes; limit 1–5. Response is a bounded list of external titles, HTTPS URLs and snippets. Invalid URLs in search results are discarded, rather than considered safe to fetch.
- `web_reader({"url":"https://docs.rust-lang.org/..."})`: URL up to 2048 bytes, must be HTTPS on port 443 with no username/password/fragment, and hostname must exactly match the explicit reader allowlist. Only text/plain, text/html and application/xhtml+xml are returned, capped at 6 KiB of normalized text; scripts, styles and hidden SVG/noscript elements are skipped by a minimal text extractor. It is **not** a complete browser/DOM reader or JavaScript renderer.
- Tool outputs are labeled **untrusted external data, not instructions**, wrapped by the existing provider-continuation boundary and never turned directly into effects or persisted character messages. Only a final valid `StructuredResponse` can propose state/memory/game effects.
- Tool failures reject the interim generation (visible as a generation failure) rather than silently trying different endpoints, returning a fabricated result or accidentally executing another tool.

## Network security envelope

1. Reject HTTP, IP-literal/localhost/internal domains, non-443 ports, URL credentials, fragments and invalid/overlong hostnames.
2. For each tool request, resolve the **exact destination hostname**, reject empty DNS answers and **any** non-public IP in the result, including private, loopback, link-local, carrier NAT, reserved/documentation, IPv4-mapped and IPv6 transition addresses. DNS is bounded to 3 seconds.
3. Pin the accepted resolved socket addresses in a **new dedicated reqwest client** with normal TLS hostname/certificate validation, HTTPS-only, `no_proxy()`, no redirects and a fixed agent. This avoids a DNS-rebinding window between validation and connection. Neither system proxy nor HTTP redirects can route to another host.
4. Limit connection setup to 3 seconds, whole HTTP exchange to 8 seconds after DNS, and response bytes to 128 KiB (both declared Content-Length and streamed chunks). Reject non-2xx statuses, redirects, binary/media formats and invalid UTF-8; do not log bodies or credentials.
5. Tool limits and cancellation use the same existing `ResponseGenerator` request lifecycle; no retries, cookies, stateful sessions or external mutations are added.

These mitigations are deliberately restrictive. A corporate HTTPS URL pointing to an internal network is rejected even when it matches an allowlisted hostname. If an instance serves through a proxy or private address, it is not usable through these tools. Additional environments require their own separately reviewed policy, not a hidden bypass.

## Test evidence and limitations

`src/generation/web_tools.rs` unit tests cover URL/host parsing, private/metadata/IPv6-denial, explicit settings gates, bounded search-result formatting, basic HTML extraction and reader allowlist rejection *before* DNS. `tests/tool_continuation.rs` covers rejection of a private-host tool call through the real request lifecycle, without Timeline acceptance. Existing mock provider tests cover continuation, cancellation and native OpenAI/Gemini paths.

**Not claimed:** real SearXNG server compatibility, live HTTPS request/redirect integration tests with external connectivity, human UI approval flows, page summarization quality, PDF reading, screenshotting, scraping of dynamic content, and multi-call native parallelism. #30 remains open until these scenarios are verified where required.
