// Private Docs development transport. Production registries never import this module.
const contextsKey = Symbol.for("wake.docs.development.v1");

export function docsDevContext(basePath) {
  const contexts = globalThis[contextsKey] || (globalThis[contextsKey] = new Map());
  let context = contexts.get(basePath);
  if (!context) {
    context = { shared: new Map(), modules: new Map(), pending: new Map(), requested: new Set(), updates: null };
    contexts.set(basePath, context);
  }
  return context;
}

export function publishDevModule(basePath, key, value) {
  docsDevContext(basePath).modules.set(key, value);
}

export function loadDevModule(basePath, key) {
  const context = docsDevContext(basePath);
  if (context.modules.has(key)) return Promise.resolve(context.modules.get(key));
  if (context.pending.has(key)) return context.pending.get(key);
  context.requested.add(key);
  const url = basePath.replace(/\/$/, "") + "/@wake/docs/" + key + "/bundle.js";
  const pending = (async () => {
    await watchDevModules(context);
    // Check the compilation response before creating a script so diagnostics reach the page.
    const response = await fetch(url);
    if (!response.ok) throw new Error("Docs compilation failed (" + response.status + "): " + await response.text());
    await new Promise((resolve, reject) => {
      const script = document.createElement("script");
      script.src = url;
      const nonce = document.querySelector("script[nonce]")?.nonce;
      if (nonce) script.nonce = nonce;
      script.onload = () => { script.remove(); resolve(); };
      script.onerror = () => { script.remove(); reject(new Error("Unable to load Docs module: " + key)); };
      document.head.appendChild(script);
    });
    if (!context.modules.has(key)) throw new Error("Docs module did not initialize: " + key);
    return context.modules.get(key);
  })();
  context.pending.set(key, pending);
  pending.then(() => context.pending.delete(key), () => context.pending.delete(key));
  return pending;
}

function watchDevModules(context) {
  if (context.updates) return context.updates;
  context.updates = new Promise((resolve, reject) => {
    const protocol = location.protocol === "https:" ? "wss:" : "ws:";
    const socket = new WebSocket(protocol + "//" + location.host + "/__wake_live_reload");
    let opened = false;
    socket.onopen = () => { opened = true; resolve(); };
    socket.onmessage = (event) => {
      let message;
      try { message = JSON.parse(event.data); } catch { return; }
      if (message.type === "reload" && typeof message.mount === "string" &&
          message.mount.startsWith("docs:") && context.requested.has(message.mount.slice(5))) {
        location.reload();
      }
    };
    socket.onclose = () => {
      context.updates = null;
      if (opened) {
        // A disconnected observer cannot prove it saw every update to an executed bundle.
        setTimeout(() => location.reload(), 1000);
      } else {
        reject(new Error("Docs update connection failed"));
      }
    };
    socket.onerror = () => socket.close();
  });
  return context.updates;
}
