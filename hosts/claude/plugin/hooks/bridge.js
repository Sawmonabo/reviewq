async function nativeArgv($) {
  const home = await $.env.get("HOME");
  const binary = await $.env.get("REVIEWQ_NATIVE_BIN");
  if (!home?.startsWith("/") || (binary && !binary.startsWith("/"))) {
    return undefined;
  }
  return [binary || `${home}/.local/bin/reviewq`, "internal"];
}

// `state` names the session and carries the companion's proof: a secret made with its
// generation that lives only in this module's memory. It reaches the helper on standard input,
// never in argv or the environment, which other processes of the same user can read.
function withProof(state, value) {
  return state.proof ? { ...value, proof: state.proof } : value;
}

export async function invoke($, action, state, value, timeoutMs = 5000) {
  try {
    const argv = await nativeArgv($);
    if (!argv) return { error: "ReviewQ could not resolve the installed native helper." };
    let stdin;
    if (action === "registration") {
      argv.push("claude-companion", state.id);
      if (value) argv.push("--target", value);
    } else {
      argv.push("claude-bridge", action, state.id);
      stdin = JSON.stringify(withProof(state, value));
    }
    const result = await $.process.run(argv, { stdin, timeoutMs, env: await $.process.hookEnv() });
    if (result.exitCode === 0 && !result.isStdoutTruncated) {
      return JSON.parse(result.stdout);
    }
    // The helper starts its failure line with `reviewq: `. The host already prints the command's
    // output under the command's name, and a tool result answers a ReviewQ tool, so the prefix
    // is dropped.
    return { error: result.stderr.trim().replace(/^reviewq: /, "").slice(0, 1024) || "ReviewQ is unavailable." };
  } catch {
    return { error: "ReviewQ is unavailable. Check the installed service and plugin." };
  }
}

export async function waitForQuestion($, state, address, questions) {
  const argv = await nativeArgv($);
  if (!argv) return undefined;
  argv.push("claude-bridge", "question", state.id);
  const input = JSON.stringify(withProof(state, { address, input: { questions } }));
  const stream = $.process.spawn({ argv, input, env: await $.process.hookEnv() });
  let stdout = "";
  for await (const chunk of stream) {
    if (chunk.stream !== "stdout") continue;
    stdout += chunk.text;
    if (stdout.length > 64 * 1024) return undefined;
  }
  return stdout ? JSON.parse(stdout).data : undefined;
}
