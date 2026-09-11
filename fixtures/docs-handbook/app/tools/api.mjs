import { createServer } from "node:http";

createServer((request, response) => {
  response.setHeader("Content-Type", "application/json");
  response.end(JSON.stringify({ path: request.url, ok: request.url === "/health" }));
}).listen(3000, "127.0.0.1");
