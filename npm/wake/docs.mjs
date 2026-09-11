const slots = new Set([
  'Root', 'Layout', 'Header', 'Navigation', 'MobileNavigation', 'SearchDialog',
  'TableOfContents', 'Page', 'Demo', 'CodeBlock', 'ApiTable',
  'PageLoading', 'PageError', 'NotFound',
]);
const reactComponents = new Set([
  Symbol.for('react.memo'), Symbol.for('react.forward_ref'), Symbol.for('react.lazy'),
]);

/** Validate a partial rendering registry without evaluating its components. */
export function defineDocsUI(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value) ||
      ![Object.prototype, null].includes(Object.getPrototypeOf(value))) {
    throw new TypeError('Wake Docs UI must be a component registry object');
  }
  const result = {};
  for (const key of Reflect.ownKeys(value)) {
    const property = Object.getOwnPropertyDescriptor(value, key);
    if (!slots.has(key) || !property || !('value' in property)) {
      throw new TypeError(`Wake Docs UI: invalid slot ${String(key)}`);
    }
    const component = property.value;
    if (component === undefined) continue;
    if (typeof component !== 'function' &&
        !(component && typeof component === 'object' && reactComponents.has(component.$$typeof))) {
      throw new TypeError(`Wake Docs UI: ${key} must be a React component`);
    }
    result[key] = component;
  }
  return Object.freeze(result);
}
