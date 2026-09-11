import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { build, createBuildContext, startDevServer, WakeError } from '@crab-dev/wake';

// 从已安装 Wake 的示例集合运行；不发布，也不调用公网接口。
const cwd = fileURLToPath(new URL('./app/', import.meta.url));
const context = await createBuildContext({ cwd, outdir: 'dist' });
try {
  const first = await context.rebuild();
  const second = await context.rebuild();
  assert.ok(first.files.some(file => file.kind === 'html'));
  assert.deepEqual(second.files, first.files);
} finally {
  await context.close();
}

await assert.rejects(
  build({ cwd, entry: 'src/does-not-exist.tsx', outdir: 'dist' }),
  error => error instanceof WakeError,
);

const server = await startDevServer({ cwd, port: 5197, open: false });
try {
  const response = await fetch(server.url);
  assert.equal(response.status, 200);
  assert.match(await response.text(), /root/);
} finally {
  await server.close();
  await server.waitUntilClosed();
}
console.log('Node API: rebuild, structured error, HTTP and resource release passed.');
