import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const fixtureDir = fileURLToPath(new URL('.', import.meta.url));

export default {
  "mode": "production",
  "entry": join(fixtureDir, "input", "entry.js"),
  "output": {
    "path": join(fixtureDir, "dist-webpack"),
    "filename": "bundle.js",
    "clean": true
  },
  "optimization": {
    "splitChunks": false,
    "minimize": true,
    "sideEffects": true
  },
  "target": "node",
  "devtool": false,
  "stats": "errors-warnings"
};
