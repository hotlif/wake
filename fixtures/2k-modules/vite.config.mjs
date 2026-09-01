import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vite';

const fixtureDir = fileURLToPath(new URL('.', import.meta.url));

export default defineConfig({
  logLevel: 'error',
  build: {
    target: ['chrome120', 'edge120', 'firefox121', 'safari17.2', 'ios17.2'],
    outDir: resolve(fixtureDir, 'dist-vite'),
    // The benchmark runner removes this directory before it starts the timer.
    emptyOutDir: false,
    sourcemap: false,
    minify: true,
    reportCompressedSize: false,
    copyPublicDir: false,
    rolldownOptions: {
      input: resolve(fixtureDir, 'input', 'entry.js'),
      output: {
        entryFileNames: 'bundle.js',
        format: 'iife',
        codeSplitting: false,
      },
    },
  },
});
