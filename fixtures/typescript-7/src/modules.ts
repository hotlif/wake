import metadata from './data.json' with { type: 'json' }
import { rowValue, type Row } from './shared.js'

export { rowValue, type Row }
export type { SharedValue } from './shared.js'

export const fixtureName = metadata.name
export const loadedValue = (await import('./dynamic.js')).dynamicValue
export const loadAgain = () => import('./dynamic.js')
