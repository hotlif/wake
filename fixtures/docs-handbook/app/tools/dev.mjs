import { startDevServer } from "@crab-dev/wake";

const server = await startDevServer({ cwd: process.cwd(), port: 5173, open: false });
const stop = () => { void server.close(); };
process.once("SIGINT", stop);
process.once("SIGTERM", stop);
try {
  console.log(server.url);
  server.on("diagnostic", diagnostic => console.error(diagnostic));
  await server.waitUntilClosed();
} finally {
  process.removeListener("SIGINT", stop);
  process.removeListener("SIGTERM", stop);
  await server.close();
}
