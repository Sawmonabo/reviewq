const ID_SEGMENT = "(?:[A-Za-z0-9_.!~*'()-]|%[0-9a-fA-F]{2})+";
const QUOTE_RESOLUTION = new RegExp(
  `^/api/v1/runs/${ID_SEGMENT}/report/versions/${ID_SEGMENT}/quote/resolve$`,
);

export function devApiProxy(frontend, backend, readOnly) {
  let proxy;
  return {
    config: {
      target: "http://127.0.0.1:1",
      changeOrigin: true,
      configure(server) {
        proxy = server;
        proxy.on("proxyReq", (outbound, request) => {
          const origin = frontend();
          const target = `http://${outbound.getHeader("host")}`;
          if (request.headers.origin === origin) outbound.setHeader("origin", target);
          const referer = request.headers.referer;
          if (referer?.startsWith(`${origin}/`))
            outbound.setHeader("referer", `${target}${referer.slice(origin.length)}`);
          else outbound.removeHeader("referer");
        });
      },
    },
    allow(request, response) {
      const origin = frontend();
      const host = request.headersDistinct.host;
      const origins = request.headersDistinct.origin;
      if (
        host?.length !== 1 ||
        host[0] !== new URL(origin).host ||
        (origins && (origins.length !== 1 || origins[0] !== origin))
      )
        return fail(
          response,
          403,
          "forbidden",
          "Open the development frontend at its loopback address.",
        );
      const path = request.url?.split("?", 1)[0] ?? "/";
      if (!path.startsWith("/api/")) return true;
      if (!path.startsWith("/api/v1/"))
        return fail(response, 404, "not_found", "Unknown ReviewQ API path.");
      if (
        readOnly &&
        !["GET", "HEAD", "OPTIONS"].includes(request.method) &&
        !(
          request.method === "POST" &&
          origins &&
          QUOTE_RESOLUTION.test(path) &&
          new URL(path, origin).pathname === path
        )
      )
        return fail(response, 403, "forbidden", "The development frontend is in read-only mode.");
      const target = backend();
      if (!target)
        return fail(
          response,
          503,
          "unavailable",
          "The development backend is starting or needs a successful build.",
        );
      proxy.options.target = target;
      return true;
    },
  };
}

function fail(response, status, code, message) {
  response.writeHead(status, { "Content-Type": "application/json", "Cache-Control": "no-store" });
  response.end(JSON.stringify({ error: { code, message, retryable: status >= 500 } }));
  return false;
}
