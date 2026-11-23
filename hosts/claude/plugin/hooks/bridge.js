async function nativeArgv($) {
  const home = await $.env.get("HOME");
  const binary = await $.env.get("REVIEWQ_NATIVE_BIN");
  if (!home?.startsWith("/") || (binary && !binary.startsWith("/"))) {
    return undefined;
  }
  return [binary || `${home}/.local/bin/reviewq`, "internal"];
}

export async function invoke($, action, id, value) {
  try {
    const argv = await nativeArgv($);
    if (!argv) return { error: "ReviewQ could not resolve the installed native helper." };
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

export async function waitForQuestion($, id, address, questions) {
  const argv = await nativeArgv($);
  if (!argv) return undefined;
  argv.push("claude-bridge", "question", id);
  const stream = $.process.spawn({ argv, input: JSON.stringify({ address, input: { questions } }), env: await $.process.hookEnv() });
  let stdout = "";
  for await (const chunk of stream) {
    if (chunk.stream !== "stdout") continue;
    stdout += chunk.text;
    if (stdout.length > 64 * 1024) return undefined;
  }
  return stdout ? JSON.parse(stdout).data : undefined;
}
