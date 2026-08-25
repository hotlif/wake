import GlobalWindow from './lib/window/GlobalWindow.js';
import * as PropertySymbol from './lib/PropertySymbol.js';
import { installWakeHappyDomAdapter } from './wake-happy-dom-adapter.js';

const IGNORE = new Set(['constructor', 'undefined', 'NaN', 'global', 'globalThis']);
let registration = null;

export const happyDomVersion = '20.11.6';

export function installWakeDom(options = {}) {
    if (registration) {
        throw Object.assign(new Error('Wake DOM is already installed for this suite.'), {
            code: 'WAKE_TEST_RUNTIME'
        });
    }
    const window = new GlobalWindow({
        url: options.url || 'http://wake.test/',
        width: options.width || 1280,
        height: options.height || 720,
        console: globalThis.console,
        settings: {
            disableJavaScriptEvaluation: true,
            enableJavaScriptEvaluation: false,
            disableJavaScriptFileLoading: true,
            disableCSSFileLoading: true,
            enableImageFileLoading: false,
            disableIframePageLoading: true,
            enableFileSystemHttpRequests: false,
            handleDisabledFileLoadingAsSuccess: false,
            navigation: {
                disableMainFrameNavigation: true,
                disableChildFrameNavigation: true,
                disableChildPageNavigation: true,
                disableFallbackToSetURL: true
            },
            viewport: {
                width: options.width || 1280,
                height: options.height || 720,
                devicePixelRatio: options.deviceScaleFactor || 1
            },
            device: {
                prefersColorScheme: options.colorScheme || 'light',
                prefersReducedMotion: options.reducedMotion || 'no-preference',
                mediaType: 'screen',
                forcedColors: 'none'
            }
        }
    });
    installWakeHappyDomAdapter(window, PropertySymbol);
    registration = new Map();
    for (const key of Reflect.ownKeys(window)) {
        if (typeof key === 'string' && IGNORE.has(key)) continue;
        const descriptor = Object.getOwnPropertyDescriptor(window, key);
        if (!descriptor) continue;
        const current = Object.getOwnPropertyDescriptor(globalThis, key);
        if (typeof key === 'string' && current && current.value !== undefined && current.value === descriptor.value) {
            continue;
        }
        if (current && current.configurable === false) {
            continue;
        }
        registration.set(key, current || null);
        if ('value' in descriptor && descriptor.value === window) descriptor.value = globalThis;
        Object.defineProperty(globalThis, key, { ...descriptor, configurable: true });
    }
    for (const key of ['window', 'self']) {
        if (!registration.has(key)) {
            registration.set(key, Object.getOwnPropertyDescriptor(globalThis, key) || null);
        }
        Object.defineProperty(globalThis, key, {
            value: globalThis,
            writable: true,
            enumerable: true,
            configurable: true
        });
    }
    globalThis.document[PropertySymbol.defaultView] = globalThis;
    globalThis.IS_REACT_ACT_ENVIRONMENT = true;
    const deniedNetwork = () => Promise.reject(Object.assign(
        new Error('External network access is denied. Register the request with network.route() or network.allow().'),
        { code: 'WAKE_TEST_NETWORK' }
    ));
    Object.defineProperty(globalThis, 'fetch', {
        value: deniedNetwork,
        writable: true,
        configurable: true
    });
    return globalThis;
}

export async function closeWakeDom() {
    if (!registration) return;
    const happyDOM = globalThis.happyDOM;
    for (const [key, descriptor] of registration) {
        if (descriptor) Object.defineProperty(globalThis, key, descriptor);
        else Reflect.deleteProperty(globalThis, key);
    }
    registration = null;
    Reflect.deleteProperty(globalThis, 'IS_REACT_ACT_ENVIRONMENT');
    if (happyDOM) await happyDOM.close();
}

installWakeDom(globalThis.__wakeDomOptions || {});
