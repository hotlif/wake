import './src/main.js';

const reg = globalThis.__reg || {};
const keys = Object.keys(reg).sort();
let hash = 0;
for (const k of keys) {
  const v = reg[k];
  hash = ((hash << 5) - hash + v + k.length) >>> 0;
}
console.log('modules=' + keys.length + ' hash=' + hash);