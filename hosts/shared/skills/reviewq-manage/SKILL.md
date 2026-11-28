---
name: reviewq-manage
description: Find registered ReviewQ work, read its published reports and findings or a reviewing agent's transcript, or send the user's message to a confirmed lead agent through the configured ReviewQ tools.
---

Use ReviewQ's MCP tools from this authorized checkout. Find the review with search_reviews: give query to match its merge request, ticket or a finding's title, and filter to narrow it by stage. Open it with get_review to confirm the exact run and lead agent. To read what an agent published, list the run's report versions with get_review and report kind versions, then read one version's summary or text with report kind summary or section and its receiptId, or its findings with get_findings, which takes the same target, sessionKey and receiptId and filters by state and kind. A search hit found by a finding title already names the run's sessionKey and the version's receiptId for get_findings. These tools read published report versions only.

Use read_agent_transcript for the selected agent and follow its continuation when needed. To find something the agent said, use read kind search with a query, then read kind page with aroundId set to a match's entryId to see it in place.

Use send_agent_message only when the user asks you to send a message to a confirmed lead agent. Preserve the user's text, create a new submission UUID once for that message, and reuse it only to retry the same message. Report the returned receipt and distinguish a queued delivery from a confirmed native entry. When the requested review or agent is ambiguous, ask the user to select it.
