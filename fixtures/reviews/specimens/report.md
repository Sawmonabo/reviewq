# Worked review

## Verdict

Request changes until the parser validates an empty instruction.

## Finding

The parser accepts **one** &amp; two 🧪 before checking that input is complete.

> The original instruction remains visible in the terminal.

~~~mermaid
flowchart LR
  Input --> Parse --> Check --> Result
~~~
