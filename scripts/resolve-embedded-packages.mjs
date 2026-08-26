import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

const workspace = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const loaderPath = path.join(workspace, ".pnp.cjs");
if (!fs.existsSync(loaderPath)) {
  throw new Error(`missing ${loaderPath}; run yarn install --immutable`);
}

const require = createRequire(import.meta.url);
const pnpapi = require(loaderPath);
const args = process.argv.slice(2);
if (args.length === 0 || args.length % 2 !== 0) {
  throw new Error("expected package/version argument pairs");
}

for (let index = 0; index < args.length; index += 2) {
  const name = args[index];
  const expectedVersion = args[index + 1];
  const metadataPath = pnpapi.resolveToUnqualified(
    `${name}/package.json`,
    fileURLToPath(import.meta.url),
  );
  const root = fs.realpathSync(path.dirname(metadataPath));
  const portableRoot = root.replaceAll("\\", "/");
  if (portableRoot.includes(".zip/")) {
    throw new Error(
      `${name}@${expectedVersion} resolved inside a zip; mark it unplugged in dependenciesMeta`,
    );
  }
  const metadata = JSON.parse(fs.readFileSync(path.join(root, "package.json"), "utf8"));
  if (metadata.name !== name || metadata.version !== expectedVersion) {
    throw new Error(
      `expected ${name}@${expectedVersion}, found ${metadata.name}@${metadata.version}`,
    );
  }
  process.stdout.write(`${name}\t${root}\n`);
}
