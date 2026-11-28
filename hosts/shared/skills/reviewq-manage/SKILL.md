---
name: reviewq-manage
description: Find registered ReviewQ work, read a reviewing agent's transcript, or send the user's message to a confirmed lead agent through the configured ReviewQ tools.
---

Use ReviewQ's MCP tools from this authorized checkout. Find the review with search_reviews and confirm the exact run and lead agent with get_review. Use read_agent_transcript for the selected agent and follow its continuation when needed.

Use send_agent_message only when the user asks you to send a message to a confirmed lead agent. Preserve the user's text, create a new submission UUID once for that message, and reuse it only to retry the same message. Report the returned receipt and distinguish a queued delivery from a confirmed native entry. When the requested review or agent is ambiguous, ask the user to select it.
