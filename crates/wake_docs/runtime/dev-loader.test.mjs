import { test, expect } from "@crab-dev/wake/test";
import { docsDevContext, loadDevModule, publishDevModule } from "./dev-loader.mjs";

async function transport(run) {
  const saved = {};
  for (const key of ["document", "fetch", "WebSocket", "location"]) saved[key] = Object.getOwnPropertyDescriptor(globalThis, key);
  const state = { fetches: 0, scripts: 0, fail: false, publish: true, reloads: 0, sockets: [], value: { default: { component: true } } };
  Object.defineProperty(globalThis, "location", { configurable: true, value: { protocol: "http:", host: "localhost:5173", reload() { state.reloads++; } } });
  Object.defineProperty(globalThis, "WebSocket", { configurable: true, value: class {
    constructor() { state.sockets.push(this); Promise.resolve().then(() => this.onopen()); }
    close() { this.onclose(); }
  } });
  Object.defineProperty(globalThis, "fetch", { configurable: true, value: async () => {
    state.fetches++;
    return { ok: !state.fail, status: state.fail ? 503 : 200, text: async () => "source diagnostic" };
  } });
  Object.defineProperty(globalThis, "document", { configurable: true, value: {
    querySelector: () => null,
    createElement: () => ({ remove() {} }),
    head: { appendChild(script) {
      state.scripts++;
      const [base, suffix] = script.src.split("/@wake/docs/");
      if (state.publish) publishDevModule(base || "/", suffix.split("/")[0], state.value);
      Promise.resolve().then(() => script.onload());
    } },
  } });
  try { await run(state); } finally {
    for (const [key, descriptor] of Object.entries(saved)) {
      if (descriptor) Object.defineProperty(globalThis, key, descriptor);
      else delete globalThis[key];
    }
  }
}

test("Docs demand loads deduplicate and preserve shared object identity", async () => {
  await transport(async (state) => {
    const base = "/dedupe";
    const react = { useState() {} };
    docsDevContext(base).shared.set("react", react);
    const first = loadDevModule(base, "page-61");
    expect(loadDevModule(base, "page-61")).toBe(first);
    expect(await first).toBe(state.value);
    expect(await loadDevModule(base, "page-61")).toBe(state.value);
    expect(docsDevContext(base).shared.get("react")).toBe(react);
    expect(state.fetches).toBe(1);
    expect(state.scripts).toBe(1);
    expect(state.sockets.length).toBe(1);
    state.sockets[0].onmessage({ data: JSON.stringify({ type: "reload", mount: "docs:page-62" }) });
    expect(state.reloads).toBe(0);
    state.sockets[0].onmessage({ data: JSON.stringify({ type: "reload", mount: "docs:page-61" }) });
    expect(state.reloads).toBe(1);
  });
});

test("Docs compilation failures and missing exports can be retried", async () => {
  await transport(async (state) => {
    state.fail = true;
    await expect(loadDevModule("/retry", "page-61")).rejects.toThrow("source diagnostic");
    expect(state.scripts).toBe(0);
    state.fail = false;
    state.publish = false;
    await expect(loadDevModule("/retry", "page-61")).rejects.toThrow("did not initialize");
    state.publish = true;
    expect(await loadDevModule("/retry", "page-61")).toBe(state.value);
    expect(state.fetches).toBe(3);
    expect(docsDevContext("/unrelated").modules.size).toBe(0);
  });
});
