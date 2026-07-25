import { greet } from './greet.js';
import { PI } from './math.js';
const message = greet('wake');
console.log(message, 'PI=' + PI);
export const output = message + '|' + PI;
