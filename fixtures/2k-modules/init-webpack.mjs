import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = fileURLToPath(new URL('.', import.meta.url));

const config = {
  mode: 'production',
  entry: join(__dirname, 'input', 'entry.js'),
  output: {
    path: join(__dirname, 'dist-webpack'),
    filename: 'bundle.js',
    clean: true,
  },
  // 对齐 wake 默认单 bundle 模式（关闭代码分割）
  optimization: {
    splitChunks: false,
    minimize: true,
    sideEffects: true,
  },
  target: 'node',
  devtool: false,
  stats: 'errors-warnings',
};

mkdirSync(join(__dirname, 'dist-webpack'), { recursive: true });
writeFileSync(
  join(__dirname, 'webpack.config.mjs'),
  `export default ${JSON.stringify(config, null, 2)};\n`,
  'utf8',
);
console.log('Created webpack.config.mjs');
