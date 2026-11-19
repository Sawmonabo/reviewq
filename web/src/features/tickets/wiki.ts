export function jiraWiki(source: string): string {
  const chunks = source.split(/(\{(?:code|noformat)(?::[^}\r\n]*)?\}[\s\S]*?\{(?:code|noformat)\})/g);
  return chunks.map((chunk, index) => {
    if (index % 2) {
      const fence = "````";
      return chunk.replace(/^\{(?:code|noformat)(?::([^}\r\n]*))?\}/, (_match, language: string | undefined) => `${fence}${/^[\w-]+$/.test(language ?? "") ? language : ""}\n`)
        .replace(/\{(?:code|noformat)\}$/, `\n${fence}\n`);
    }
    return chunk
      .replace(/^h([1-6])\.\s+/gm, (_match, level: string) => `${"#".repeat(Number(level))} `)
      .replace(/^([*#]+)\s+/gm, (_match, prefix: string) => `${"  ".repeat(prefix.length - 1)}${prefix.endsWith("#") ? "1." : "-"} `)
      .replace(/\[([^|\]\r\n]+)\|(https?:[^\]\r\n]+)\]/g, "[$1]($2)")
      .replace(/\{\{([^}\r\n]+)\}\}/g, "`$1`")
      .replace(/^\|\|([^\r\n]*)\|\|$/gm, (_match, cells: string) => {
        const values = cells.split("||");
        return `| ${values.join(" | ")} |\n| ${values.map(() => "---").join(" | ")} |`;
      });
  }).join("");
}
