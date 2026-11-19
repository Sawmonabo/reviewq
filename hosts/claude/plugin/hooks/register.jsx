let generation = 0;
let binding;
let checking = false;
let recheck = false;

async function companion($, id, target) {
  try {
    const home = await $.env.get("HOME");
    if (!home?.startsWith("/")) return { error: "ReviewQ could not resolve the native account." };
    const argv = [`${home}/.local/bin/reviewq`, "internal", "claude-companion", id];
    if (target) argv.push("--target", target);
    const result = await $.process.run(argv);
    if (result.exitCode === 0) return JSON.parse(result.stdout);
    return { error: result.stderr.trim().slice(0, 4096) || "ReviewQ is unavailable." };
  } catch {
    return { error: "ReviewQ is unavailable. Check the installed ReviewQ service and CLI." };
  }
}

async function refresh($, target) {
  const sequence = ++generation;
  const id = await $.session.id();
  const previous = binding?.id === id && binding.linked;
  const result = await companion($, id, target);
  if (sequence === generation && await $.session.id() === id) {
    binding = { id, linked: result.linked || (previous && !!result.error), stale: !!result.error };
    $.ui.invalidate("ui.render");
  }
  return result;
}

async function checkRegistration($) {
  if (checking) {
    recheck = true;
    return;
  }
  checking = true;
  try {
    do {
      recheck = false;
      await refresh($);
    } while (recheck);
  } finally {
    checking = false;
  }
}

export function register(on) {
  on("session.start", async ($, event, next) => {
    await $.command.register({
      name: "reviewq",
      description: "Show ReviewQ status or link this session to a merge request",
      argumentHint: "[merge request or ticket]",
    });
    $.clock.after(0, async () => { await checkRegistration($); });
    return next(event);
  });

  on("command.run", { command: "reviewq" }, async ($, event) => {
    const target = (event.args ?? "").trim();
    const result = await refresh($, target);
    if (target && result.linked) $.clock.after(0, async () => { await checkRegistration($); });
    return { text: result.message ?? result.error, isTextDisplayOnly: true };
  });

  on("prompt.submit", async ($, event, next) => {
    $.clock.after(0, async () => { await checkRegistration($); });
    return next(event);
  });

  on("session.end", ($, event, next) => {
    generation += 1;
    binding = undefined;
    return next(event);
  });

  on("ui.render", { component: "AbovePrompt" }, ($, event, next) => {
    if (!binding?.linked) return next(event);
    const { Text } = $.ui.resolve(event);
    return { view: <Text dimColor>{binding.stale ? "ReviewQ · offline" : "ReviewQ · linked"}</Text> };
  });
}
