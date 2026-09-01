import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import webpack from 'webpack';

const fixtureDir = fileURLToPath(new URL('.', import.meta.url));

export default {
  mode: 'production',
  entry: join(fixtureDir, 'input', 'entry.js'),
  target: 'browserslist:chrome 120, edge 120, firefox 121, safari 17.2, ios_saf 17.2',
  cache: false,
  output: {
    path: join(fixtureDir, 'dist-webpack'),
    filename: 'bundle.js',
    // The benchmark runner removes this directory before it starts the timer.
    clean: false,
    iife: true,
  },
  optimization: {
    splitChunks: false,
    runtimeChunk: false,
    minimize: true,
    sideEffects: true,
  },
  plugins: [
    new webpack.optimize.LimitChunkCountPlugin({ maxChunks: 1 }),
  ],
  devtool: false,
  stats: 'errors-warnings',
};
