export async function invoke($, action, id, value) {
  try {
    const home = await $.env.get("HOME");
    const binary = await $.env.get("REVIEWQ_NATIVE_BIN");
    if (!home?.startsWith("/") || (binary && !binary.startsWith("/"))) {
      return { error: "ReviewQ could not resolve the installed native helper." };
    }
    const argv = [binary || `${home}/.local/bin/reviewq`, "internal"];
    let stdin;
    if (action === "registration") {
      argv.push("claude-companion", id);
      if (value) argv.push("--target", value);
    } else {
      argv.push("claude-bridge", action, id);
      stdin = JSON.stringify(value);
    }
    const result = await $.process.run(argv, { stdin, timeoutMs: 5000, env: await $.process.hookEnv() });
    if (result.exitCode === 0 && !result.isStdoutTruncated) {
      return JSON.parse(result.stdout);
    }
    return { error: result.stderr.trim().slice(0, 1024) || "ReviewQ is unavailable." };
  } catch {
    return { error: "ReviewQ is unavailable. Check the installed service and plugin." };
  }
}
