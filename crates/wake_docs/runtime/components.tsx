import React, { useEffect, useMemo, useRef, useState } from "react";
import { demos } from "@wake/docs/registry.ts";
import { siteConfig } from "@wake/docs/config.tsx";

type DemoRecord = (typeof demos)[number];
type Viewport = "responsive" | "tablet" | "mobile";
type Theme = "light" | "dark" | "system";
type Args = Record<string, unknown>;

const isChinese = siteConfig.locale.toLowerCase().startsWith("zh");
const text = (english: string, chinese: string) => isChinese ? chinese : english;

function isPlainObject(value: unknown): value is Args {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

function serializableArgs(value: unknown): { args: Args; warning?: string } {
  if (!isPlainObject(value)) return { args: {} };
  try {
    const encoded = JSON.stringify(value, (_key, next) => {
      if (["function", "symbol", "bigint", "undefined"].includes(typeof next)) {
        throw new TypeError("meta.args contains a non-JSON value");
      }
      return next;
    });
    return { args: JSON.parse(encoded) as Args };
  } catch {
    return {
      args: {},
      warning: text("meta.args must contain JSON-serializable values.", "meta.args 只能包含可 JSON 序列化的值。"),
    };
  }
}

function controlDefaults(demo: DemoRecord): Args {
  return Object.fromEntries(
    demo.controls
      .filter((control) => control.defaultValue !== undefined)
      .map((control) => [control.name, control.defaultValue]),
  );
}

function equalValue(left: unknown, right: unknown): boolean {
  if (Object.is(left, right)) return true;
  try {
    return JSON.stringify(left) === JSON.stringify(right);
  } catch {
    return false;
  }
}

function copyText(value: string): Promise<void> {
  if (navigator.clipboard?.writeText) return navigator.clipboard.writeText(value);
  const textarea = document.createElement("textarea");
  textarea.value = value;
  textarea.setAttribute("readonly", "");
  textarea.style.position = "fixed";
  textarea.style.opacity = "0";
  document.body.appendChild(textarea);
  textarea.select();
  try {
    return document.execCommand("copy")
      ? Promise.resolve()
      : Promise.reject(new Error("copy unavailable"));
  } catch (reason) {
    return Promise.reject(reason);
  } finally {
    textarea.remove();
  }
}

function changedArgs(args: Args, defaults: Args): Args {
  return Object.fromEntries(
    Object.entries(args).filter(([name, value]) => !equalValue(value, defaults[name])),
  );
}

function readLocation(): { id?: string; args: Args; viewport: Viewport } {
  const match = window.location.hash.match(/^#\/components\/([^?]*)(?:\?(.*))?$/);
  if (!match) return { args: {}, viewport: "responsive" };
  let args: Args = {};
  const params = new URLSearchParams(match[2] || "");
  try {
    const parsed = JSON.parse(params.get("args") || "{}");
    if (isPlainObject(parsed)) args = parsed;
  } catch {
    args = {};
  }
  const rawViewport = params.get("viewport");
  const viewport = rawViewport === "tablet" || rawViewport === "mobile"
    ? rawViewport
    : "responsive";
  return { id: decodeURIComponent(match[1]), args, viewport };
}

function locationHash(id: string, args: Args, defaults: Args, viewport: Viewport): string {
  const params = new URLSearchParams();
  const changed = changedArgs(args, defaults);
  if (Object.keys(changed).length) params.set("args", JSON.stringify(changed));
  if (viewport !== "responsive") params.set("viewport", viewport);
  const query = params.toString();
  return "#/components/" + encodeURIComponent(id) + (query ? "?" + query : "");
}

function useTheme() {
  const [theme, setTheme] = useState<Theme>(() => {
    const saved = localStorage.getItem("wake-docs-theme");
    return saved === "light" || saved === "dark" || saved === "system"
      ? saved
      : siteConfig.defaultTheme as Theme;
  });
  const resolve = (value: Theme) => value === "system"
    ? (window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light")
    : value;
  const [resolved, setResolved] = useState<"light" | "dark">(() => resolve(theme));
  useEffect(() => {
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const update = () => setResolved(resolve(theme));
    update();
    media.addEventListener("change", update);
    return () => media.removeEventListener("change", update);
  }, [theme]);
  useEffect(() => {
    localStorage.setItem("wake-docs-theme", theme);
    document.documentElement.lang = siteConfig.locale;
    document.documentElement.dataset.theme = resolved;
    document.documentElement.style.setProperty("--wake-accent", siteConfig.accentColor);
  }, [theme, resolved]);
  return { theme, resolved, setTheme };
}

function JsonControl({
  value,
  setValue,
}: {
  value: unknown;
  setValue: (value: unknown) => void;
}) {
  const [draft, setDraft] = useState(() => JSON.stringify(value ?? null, null, 2));
  const [error, setError] = useState("");
  useEffect(() => {
    setDraft(JSON.stringify(value ?? null, null, 2));
    setError("");
  }, [value]);
  const commit = () => {
    try {
      setValue(JSON.parse(draft));
      setError("");
    } catch (reason) {
      setError(String(reason));
    }
  };
  return <>
    <textarea value={draft} onChange={(event) => setDraft(event.target.value)} onBlur={commit} />
    {error && <small className="workbench-control-error">{error}</small>}
  </>;
}

function Control({
  control,
  value,
  setValue,
  clear,
}: {
  control: DemoRecord["controls"][number];
  value: unknown;
  setValue: (value: unknown) => void;
  clear: () => void;
}) {
  let input: React.ReactNode;
  if (control.kind === "boolean") {
    input = <input type="checkbox" checked={Boolean(value)} onChange={(event) => setValue(event.target.checked)} />;
  } else if (control.kind === "string") {
    input = <input type="text" value={typeof value === "string" ? value : ""} onChange={(event) => setValue(event.target.value)} />;
  } else if (control.kind === "number") {
    input = <input type="number" value={typeof value === "number" ? value : ""} onChange={(event) => setValue(event.target.value === "" ? undefined : Number(event.target.value))} />;
  } else if (control.kind === "select") {
    input = <select value={value === undefined ? 'undefined' : JSON.stringify(value)} onChange={(event) => setValue(event.target.value === 'undefined' ? undefined : JSON.parse(event.target.value))}>
      {!control.required && <option value="undefined">{text("Unset", "未设置")}</option>}
      {control.options.map((option) => <option key={JSON.stringify(option)} value={JSON.stringify(option)}>{String(option)}</option>)}
    </select>;
  } else if (control.kind === "json") {
    input = <JsonControl value={value} setValue={setValue} />;
  } else {
    input = <div className="workbench-readonly">{control.typeText}</div>;
  }
  return <div className={"workbench-control" + (control.deprecated ? " is-deprecated" : "")}>
    <div className="workbench-control-heading">
      <strong>{control.name}{control.required && <i>*</i>}</strong>
      <code>{control.typeText}</code>
      {!control.required && control.kind !== "readonly" && <button type="button" onClick={clear}>{text("Unset", "清除")}</button>}
    </div>
    {input}
    {control.description && <p>{control.description}</p>}
    {control.deprecated && <p className="workbench-deprecated">{text("Deprecated", "已废弃")}: {control.deprecated}</p>}
  </div>;
}

export function ComponentsApp() {
  const location = readLocation();
  const initial = demos.find((demo) => demo.id === location.id) || demos[0];
  const [selectedId, setSelectedId] = useState(initial?.id || "");
  const [query, setQuery] = useState("");
  const [args, setArgs] = useState<Args>({});
  const [defaults, setDefaults] = useState<Args>({});
  const [loadedId, setLoadedId] = useState("");
  const [viewport, setViewport] = useState<Viewport>(location.viewport);
  const [source, setSource] = useState("");
  const [sourceOpen, setSourceOpen] = useState(false);
  const [runtimeWarnings, setRuntimeWarnings] = useState<string[]>([]);
  const [error, setError] = useState("");
  const [copied, setCopied] = useState(false);
  const frameRef = useRef<HTMLIFrameElement>(null);
  const { theme, resolved, setTheme } = useTheme();
  const selected = demos.find((demo) => demo.id === selectedId) || demos[0];

  const groups = useMemo(() => {
    const needle = query.trim().toLowerCase();
    const visible = demos.filter((demo) => !needle || (demo.group + " " + demo.component + " " + demo.title).toLowerCase().includes(needle));
    const result = new Map<string, Map<string, DemoRecord[]>>();
    visible.forEach((demo) => {
      if (!result.has(demo.group)) result.set(demo.group, new Map());
      const components = result.get(demo.group)!;
      if (!components.has(demo.component)) components.set(demo.component, []);
      components.get(demo.component)!.push(demo);
    });
    return result;
  }, [query]);

  useEffect(() => {
    const syncLocation = () => {
      const next = readLocation();
      if (next.id && demos.some((demo) => demo.id === next.id)) {
        setSelectedId(next.id);
        setViewport(next.viewport);
      }
    };
    window.addEventListener("hashchange", syncLocation);
    return () => window.removeEventListener("hashchange", syncLocation);
  }, []);
  useEffect(() => {
    if (!selected) return;
    setLoadedId("");
    setError("");
    let active = true;
    Promise.all([selected.load(), selected.loadSource()]).then(([module, sourceModule]) => {
      if (!active) return;
      const metaArgs = serializableArgs(module.meta?.args);
      const nextDefaults = { ...controlDefaults(selected), ...metaArgs.args };
      const currentLocation = readLocation();
      const nextArgs = { ...nextDefaults, ...(currentLocation.id === selected.id ? currentLocation.args : {}) };
      setDefaults(nextDefaults);
      setArgs(nextArgs);
      setViewport(currentLocation.id === selected.id ? currentLocation.viewport : "responsive");
      setSource(sourceModule.default || "");
      setRuntimeWarnings(metaArgs.warning ? [metaArgs.warning] : []);
      setLoadedId(selected.id);
    }).catch((reason) => {
      if (active) setError(String(reason?.stack || reason));
    });
    return () => { active = false; };
  }, [selected?.id]);

  useEffect(() => {
    if (!selected || loadedId !== selected.id) return;
    frameRef.current?.contentWindow?.postMessage({ type: "wake:args", id: selected.id, args }, "*");
    const next = locationHash(selected.id, args, defaults, viewport);
    if (window.location.hash !== next) history.replaceState(null, "", next);
  }, [selected?.id, args, defaults, viewport, loadedId]);

  useEffect(() => {
    const receive = (event: MessageEvent) => {
      if (event.source !== frameRef.current?.contentWindow || !event.data) return;
      if (event.data.type === "wake:ready") {
        frameRef.current?.contentWindow?.postMessage({ type: "wake:theme", theme: resolved }, "*");
        frameRef.current?.contentWindow?.postMessage({ type: "wake:args", id: selected?.id, args }, "*");
      }
      if (event.data.type === "wake:error") setError(String(event.data.error || text("Demo failed", "Demo 运行失败")));
    };
    window.addEventListener("message", receive);
    return () => window.removeEventListener("message", receive);
  }, [selected?.id, args, resolved]);

  useEffect(() => {
    frameRef.current?.contentWindow?.postMessage({ type: "wake:theme", theme: resolved }, "*");
  }, [resolved]);

  const choose = (demo: DemoRecord) => {
    setSelectedId(demo.id);
    setArgs({});
    setDefaults({});
    setViewport("responsive");
    history.pushState(null, "", locationHash(demo.id, {}, {}, "responsive"));
  };
  const updateArg = (name: string, value: unknown) => {
    setArgs((current) => {
      const next = { ...current };
      if (value === undefined) delete next[name];
      else next[name] = value;
      return next;
    });
  };
  const copyLink = () => copyText(window.location.href).then(() => {
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1400);
  }).catch((reason) => setError(text("Copy failed", "复制失败") + ": " + String(reason)));
  const cycleTheme = () => setTheme(theme === "system" ? "light" : theme === "light" ? "dark" : "system");
  const frameUrl = selected ? siteConfig.basePath + "?__wake_demo=" + encodeURIComponent(selected.id) : "";
  const viewportLabel = viewport === "mobile" ? "390px" : viewport === "tablet" ? "768px" : "100%";

  if (!demos.length) return <main className="workbench-empty">
    <strong>{text("No component demos found", "没有找到组件 Demo")}</strong>
    <p>{text("Add a *.demo.tsx file under the configured docs source directory.", "请在配置的文档目录中添加 *.demo.tsx 文件。")}</p>
  </main>;

  return <div className="workbench-shell">
    <aside className="workbench-sidebar">
      <div className="workbench-brand"><strong>{siteConfig.title}</strong><span>Components</span></div>
      <input className="workbench-search" value={query} onChange={(event) => setQuery(event.target.value)} placeholder={text("Search components…", "搜索组件…")} />
      <nav aria-label={text("Component demos", "组件 Demo")}>
        {Array.from(groups).map(([group, components]) => <section key={group}>
          <h2>{group}</h2>
          {Array.from(components).map(([component, items]) => <details open key={component}>
            <summary>{component}</summary>
            {items.map((demo) => <button type="button" className={demo.id === selected?.id ? "active" : ""} key={demo.id} onClick={() => choose(demo)}>{demo.title}</button>)}
          </details>)}
        </section>)}
      </nav>
    </aside>
    <main className="workbench-main">
      <header className="workbench-toolbar">
        <div><strong>{selected?.component}</strong><span>/ {selected?.title}</span></div>
        <div className="workbench-toolbar-actions">
          <div className="workbench-segmented">
            {(["responsive", "tablet", "mobile"] as Viewport[]).map((option) => <button type="button" aria-pressed={viewport === option} key={option} onClick={() => setViewport(option)}>{option === "responsive" ? text("Wide", "宽屏") : option === "tablet" ? text("Tablet", "平板") : text("Mobile", "手机")}</button>)}
          </div>
          <button type="button" onClick={() => setArgs(defaults)}>{text("Reset", "重置")}</button>
          <button type="button" onClick={cycleTheme}>{theme}</button>
          <button type="button" onClick={copyLink}>{copied ? text("Copied", "已复制") : text("Copy link", "复制链接")}</button>
        </div>
      </header>
      <div className="workbench-canvas">
        <div className={"workbench-viewport workbench-viewport-" + viewport} style={{ width: viewportLabel }}>
          {selected && <iframe key={selected.id} ref={frameRef} title={selected.title} src={frameUrl} sandbox="allow-scripts allow-same-origin" />}
        </div>
      </div>
      {Boolean(error || selected?.warnings.length || runtimeWarnings.length) && <div className="workbench-diagnostics" role="status">
        {error && <p className="error">{error}</p>}
        {selected?.warnings.map((warning) => <p key={warning}>{warning}</p>)}
        {runtimeWarnings.map((warning) => <p key={warning}>{warning}</p>)}
      </div>}
      <section className="workbench-source">
        <button type="button" onClick={() => setSourceOpen((open) => !open)} aria-expanded={sourceOpen}>{sourceOpen ? text("Hide source", "收起源码") : text("Show source", "查看源码")}</button>
        {sourceOpen && <pre><code>{source}</code></pre>}
      </section>
    </main>
    <aside className="workbench-controls">
      <div className="workbench-controls-heading"><strong>{text("Controls", "属性控件")}</strong><span>{selected?.controls.filter((control) => control.kind !== "readonly").length || 0}</span></div>
      {selected?.controls.map((control) => <Control
        key={control.name}
        control={control}
        value={args[control.name]}
        setValue={(value) => updateArg(control.name, value)}
        clear={() => updateArg(control.name, undefined)}
      />)}
      {!selected?.controls.length && <p className="workbench-no-controls">{text("Type the default demo parameter to generate controls.", "为默认导出的 Demo 参数标注类型，即可生成控件。")}</p>}
    </aside>
  </div>;
}
