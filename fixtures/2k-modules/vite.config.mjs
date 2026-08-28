import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vite';

const fixtureDir = fileURLToPath(new URL('.', import.meta.url));

export default defineConfig({
  logLevel: 'error',
  build: {
    target: 'esnext',
    outDir: resolve(fixtureDir, 'dist-vite'),
    emptyOutDir: true,
    sourcemap: false,
    minify: true,
    reportCompressedSize: false,
    copyPublicDir: false,
    rolldownOptions: {
      input: resolve(fixtureDir, 'input', 'entry.js'),
      output: {
        entryFileNames: 'bundle.js',
        format: 'es',
        codeSplitting: false,
      },
    },
  },
});
