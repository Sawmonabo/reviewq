import { createServer } from "node:http";
import { createServer as createViteServer } from "vite";

// Bind loopback once. Probing a wildcard port first can reserve it on WSL.
const http = createServer();
const vite = await createViteServer({
  server: { middlewareMode: { server: http }, ws: { server: http } },
});

http.on("request", vite.middlewares);
http.listen(Number(process.env.REVIEWQ_WEB_PORT ?? 5173), "127.0.0.1", () => {
  console.log(`ReviewQ http://127.0.0.1:${http.address().port}/`);
});
