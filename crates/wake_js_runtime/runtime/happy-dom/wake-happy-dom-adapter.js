// Wake owns this compatibility layer. The npm-installed Happy DOM sources remain byte-for-byte
// upstream; browser semantics required by React conformance are installed on the realm classes.
const outputState = new WeakMap();
const patchedOutputPrototypes = new WeakSet();
const patchedFormPrototypes = new WeakSet();

export function installWakeHappyDomAdapter(window, PropertySymbol) {
    const outputPrototype = window.HTMLOutputElement.prototype;
    if (!patchedOutputPrototypes.has(outputPrototype)) {
        patchedOutputPrototypes.add(outputPrototype);
        Object.defineProperties(outputPrototype, {
            defaultValue: {
                configurable: true,
                enumerable: false,
                get() {
                    const state = outputState.get(this);
                    return state ? state.defaultValue : this.textContent || '';
                },
                set(value) {
                    value = String(value);
                    const state = outputState.get(this);
                    if (state) state.defaultValue = value;
                    else this.textContent = value;
                }
            },
            value: {
                configurable: true,
                enumerable: false,
                get() {
                    return this.textContent || '';
                },
                set(value) {
                    if (!outputState.has(this)) {
                        outputState.set(this, { defaultValue: this.textContent || '' });
                    }
                    this.textContent = String(value);
                }
            }
        });
    }

    const formPrototype = window.HTMLFormElement.prototype;
    if (!patchedFormPrototypes.has(formPrototype)) {
        patchedFormPrototypes.add(formPrototype);
        const upstreamReset = formPrototype.reset;
        Object.defineProperty(formPrototype, 'reset', {
            configurable: true,
            writable: true,
            value() {
                const outputs = [];
                for (const element of this[PropertySymbol.getFormControlItems]()) {
                    if (element[PropertySymbol.tagName] === 'OUTPUT') {
                        outputs.push(element);
                        // Happy DOM's reset implementation reads this internal slot directly.
                        // Prepare it from the standards-facing adapter state before delegating.
                        element[PropertySymbol.defaultValue] = element.defaultValue;
                    }
                }
                const result = upstreamReset.call(this);
                for (const output of outputs) outputState.delete(output);
                return result;
            }
        });
    }
}
