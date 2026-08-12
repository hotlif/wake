import React, { useEffect, useMemo, useRef, useState } from "react";
import Alert from "@crab-dev/rc-alert";
import Button from "@crab-dev/rc-button";
import Dialog from "@crab-dev/rc-dialog";
import Drawer from "@crab-dev/rc-drawer";
import Empty from "@crab-dev/rc-empty";
import LineEdit from "@crab-dev/rc-line-edit";
import NumberEdit from "@crab-dev/rc-number-edit";
import Segmented from "@crab-dev/rc-segmented";
import Select from "@crab-dev/rc-select";
import Switch from "@crab-dev/rc-switch";
import Tag from "@crab-dev/rc-tag";
import TextEdit from "@crab-dev/rc-text-edit";
import Tooltip from "@crab-dev/rc-tooltip";
import Tree, { LoadStateType, NodeType, type Node as TreeNode } from "@crab-dev/rc-tree";
import { Check, Code2, Copy, Menu, Monitor, Moon, RotateCcw, SlidersHorizontal, Sun } from "lucide-react";
import { demos } from "@wake/docs/registry.ts";
import { siteConfig } from "@wake/docs/config.tsx";
import { applyLocationArgs, equalValue, locationHash, readLocationHash } from "./components-state.mjs";

type DemoRecord = (typeof demos)[number];
type Viewport = "responsive" | "tablet" | "mobile";
type Theme = "light" | "dark" | "system";
type Args = Record<string, unknown>;
type WorkbenchTreeNode = TreeNode & { demoId?: string; searchText: string };
type LocationState = { id?: string; args: Args; unset: string[]; viewport: Viewport };

const isChinese = siteConfig.locale.toLowerCase().startsWith("zh");
const text = (english: string, chinese: string) => isChinese ? chinese : english;
const unsetSelectValue = "__wake_unset__";

function treeNodeKey(kind: "group" | "component" | "demo", ...parts: string[]): string {
  return kind + ":" + parts.map(encodeURIComponent).join("/");
}

function demoTreeKey(id: string): string {
  return treeNodeKey("demo", id);
}

function buildTreeData(records: readonly DemoRecord[]): WorkbenchTreeNode[] {
  const catalog = new Map<string, Map<string, DemoRecord[]>>();
  records.forEach((demo) => {
    if (!catalog.has(demo.group)) catalog.set(demo.group, new Map());
    const components = catalog.get(demo.group)!;
    if (!components.has(demo.component)) components.set(demo.component, []);
    components.get(demo.component)!.push(demo);
  });

  const nodes: WorkbenchTreeNode[] = [];
  Array.from(catalog).forEach(([group, components], groupIndex) => {
    const groupNode: WorkbenchTreeNode = {
      id: treeNodeKey("group", group),
      type: NodeType.FOLDER,
      title: group,
      parent: null,
      loadState: LoadStateType.LOADING_COMPLETED,
      priority: groupIndex,
      searchText: group.toLowerCase(),
    };
    nodes.push(groupNode);

    Array.from(components).forEach(([component, items], componentIndex) => {
      const componentNode: WorkbenchTreeNode = {
        id: treeNodeKey("component", group, component),
        type: NodeType.FOLDER,
        title: component,
        parent: groupNode,
        loadState: LoadStateType.LOADING_COMPLETED,
        priority: componentIndex,
        searchText: (group + " " + component).toLowerCase(),
      };
      nodes.push(componentNode);

      items.forEach((demo, demoIndex) => {
        nodes.push({
          id: demoTreeKey(demo.id),
          type: NodeType.FILE,
          title: demo.title,
          parent: componentNode,
          loadState: LoadStateType.LOADING_COMPLETED,
          priority: demoIndex,
          demoId: demo.id,
          searchText: (demo.group + " " + demo.component + " " + demo.title).toLowerCase(),
        });
      });
    });
  });
  return nodes;
}

function useTreeViewportSize(itemCount: number, active: boolean) {
  const ref = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState(() => ({
    width: 188,
    height: Math.max(96, Math.min(480, itemCount * 32 + 8)),
  }));

  useEffect(() => {
    const element = ref.current;
    if (!element || !active) return;
    const update = () => {
      const rect = element.getBoundingClientRect();
      const width = Math.max(1, Math.floor(element.clientWidth || rect.width));
      const availableHeight = Math.max(1, Math.floor(element.clientHeight || rect.height));
      const height = Math.min(availableHeight, Math.max(96, itemCount * 32 + 8));
      setSize((current) => current.width === width && current.height === height
        ? current
        : { width, height });
    };
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(update);
    observer?.observe(element);
    window.addEventListener("resize", update);
    update();
    return () => {
      observer?.disconnect();
      window.removeEventListener("resize", update);
    };
  }, [active, itemCount]);

  return { ref, ...size };
}

function useMediaQuery(query: string): boolean {
  const [matches, setMatches] = useState(() => window.matchMedia(query).matches);
  useEffect(() => {
    const media = window.matchMedia(query);
    const update = () => setMatches(media.matches);
    update();
    media.addEventListener("change", update);
    return () => media.removeEventListener("change", update);
  }, [query]);
  return matches;
}

function encodeSelectValue(value: unknown): string {
  return JSON.stringify(value) ?? "undefined";
}

function themeLabel(theme: Theme): string {
  if (theme === "light") return text("Light", "浅色");
  if (theme === "dark") return text("Dark", "深色");
  return text("System", "跟随系统");
}

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

function readLocation(): LocationState {
  return readLocationHash(window.location.hash) as LocationState;
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
  label,
  value,
  setValue,
}: {
  label: string;
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
      setError(text("Invalid JSON: ", "JSON 格式无效：") + String(reason));
    }
  };
  return <>
    <TextEdit
      className="workbench-control-field workbench-control-field-json"
      aria-label={label}
      value={draft}
      rows={5}
      status={error ? "error" : undefined}
      onChange={(event) => setDraft(event.target.value)}
      onBlur={commit}
    />
    {error && <Alert className="workbench-diagnostics-alert workbench-control-error" type="error" showIcon>{error}</Alert>}
  </>;
}

function Control({
  control,
  value,
  defaultValue,
  setValue,
  clear,
}: {
  control: DemoRecord["controls"][number];
  value: unknown;
  defaultValue: unknown;
  setValue: (value: unknown) => void;
  clear: () => void;
}) {
  const rowRef = useRef<HTMLDivElement>(null);
  const typeLabel = control.kind === "select"
    ? text(`enum · ${control.options.length}`, `枚举 · ${control.options.length}`)
    : control.kind === "json" ? "JSON" : control.kind;
  const clearValue = () => {
    clear();
    requestAnimationFrame(() => {
      rowRef.current?.querySelector<HTMLElement>(
        'button.workbench-control-field, input.workbench-control-field, textarea.workbench-control-field, .workbench-control-field button, .workbench-control-field input, .workbench-control-field textarea, .workbench-control-field [tabindex]:not([tabindex="-1"])',
      )?.focus();
    });
  };
  let input: React.ReactNode;
  if (control.kind === "boolean") {
    input = <Switch
      className="workbench-control-field"
      aria-label={control.name}
      checked={Boolean(value)}
      onChange={(checked) => setValue(checked)}
    />;
  } else if (control.kind === "string") {
    input = <LineEdit
      className="workbench-control-field"
      aria-label={control.name}
      value={typeof value === "string" ? value : ""}
      onChange={(event) => setValue(event.target.value)}
    />;
  } else if (control.kind === "number") {
    input = <div className="workbench-control-field workbench-number-field">
      <NumberEdit
        aria-label={control.name}
        value={typeof value === "number" ? value : null}
        onChange={(next) => setValue(next === null ? undefined : next)}
      />
    </div>;
  } else if (control.kind === "select") {
    const options = [
      ...(!control.required ? [{ label: text("Unset", "未设置"), value: unsetSelectValue }] : []),
      ...control.options.map((option) => ({
        label: typeof option === "string" ? option : encodeSelectValue(option),
        value: encodeSelectValue(option),
      })),
    ];
    input = <Select
      className="workbench-control-field"
      aria-label={control.name}
      value={value === undefined ? unsetSelectValue : encodeSelectValue(value)}
      options={options}
      placeholder={text("Select a value", "请选择")}
      onChange={(encoded) => setValue(encoded === undefined || encoded === unsetSelectValue ? undefined : JSON.parse(encoded))}
    />;
  } else if (control.kind === "json") {
    input = <JsonControl label={control.name} value={value} setValue={setValue} />;
  } else {
    input = <div className="workbench-readonly">{control.typeText}</div>;
  }
  return <div ref={rowRef} className={"workbench-control" + (control.deprecated ? " is-deprecated" : "")}>
    <div className="workbench-control-heading">
      <div className="workbench-control-identity">
        <strong>{control.name}{control.required && <i>*</i>}</strong>
        <Tooltip className="workbench-type-tooltip" title={<code>{control.typeText}</code>}>
          <Tag
            className="workbench-type-tag"
            size="small"
            tabIndex={0}
            aria-label={text(
              `View the full type for ${control.name}: `,
              `查看 ${control.name} 的完整类型：`,
            ) + control.typeText}
          >{typeLabel}</Tag>
        </Tooltip>
      </div>
      {!control.required && control.kind !== "readonly" && value !== undefined && <Button
        className={"workbench-control-action" + (!equalValue(value, defaultValue) ? " is-visible" : "")}
        appearance="text"
        size="small"
        type="button"
        aria-label={text(
          `Unset ${control.name} and leave it unspecified`,
          `清除 ${control.name}，将其设为未设置`,
        )}
        onClick={clearValue}
      >{text("Unset", "清除")}</Button>}
    </div>
    {input}
    {control.description && <p>{control.description}</p>}
    {control.deprecated && <p className="workbench-deprecated">{text("Deprecated", "已废弃")}: {control.deprecated}</p>}
  </div>;
}

function DemoCatalog({
  className,
  viewportClassName,
  active,
  query,
  setQuery,
  treeData,
  setTreeData,
  expandedKeys,
  setExpandedKeys,
  treeSelectKeys,
  setTreeSelectKeys,
  searchNeedle,
  filterTreeNode,
  searchExpandedKeys,
  hasTreeMatches,
  onChoose,
}: {
  className: string;
  viewportClassName: string;
  active: boolean;
  query: string;
  setQuery: (query: string) => void;
  treeData: TreeNode[];
  setTreeData: React.Dispatch<React.SetStateAction<TreeNode[]>>;
  expandedKeys: React.Key[];
  setExpandedKeys: React.Dispatch<React.SetStateAction<React.Key[]>>;
  treeSelectKeys: React.Key[];
  setTreeSelectKeys: React.Dispatch<React.SetStateAction<React.Key[]>>;
  searchNeedle: string;
  filterTreeNode?: (node: TreeNode) => boolean;
  searchExpandedKeys: React.Key[];
  hasTreeMatches: boolean;
  onChoose: (demo: DemoRecord) => void;
}) {
  const treeViewport = useTreeViewportSize(treeData.length, active && hasTreeMatches);
  return <div className={className}>
    <LineEdit
      className="workbench-search"
      aria-label={text("Search components", "搜索组件")}
      value={query}
      allowClear
      onClear={() => setQuery("")}
      onChange={(event) => setQuery(event.target.value)}
      placeholder={text("Search components…", "搜索组件…")}
    />
    <nav aria-label={text("Component demos", "组件示例")}>
      {hasTreeMatches && <div className={viewportClassName} ref={treeViewport.ref}>
        <Tree
          className="workbench-tree"
          width={treeViewport.width}
          height={treeViewport.height}
          treeData={treeData}
          onTreeNodeChange={setTreeData}
          expandedKeys={searchNeedle ? searchExpandedKeys : expandedKeys}
          selectKeys={[...treeSelectKeys]}
          showLine
          defaultNodeHeight={32}
          filterTreeNode={filterTreeNode}
          onExpanded={({ node }) => {
            if (searchNeedle) return;
            setExpandedKeys((current) => current.includes(node.id)
              ? current.filter((key) => key !== node.id)
              : [...current, node.id]);
          }}
          onSelect={({ event, node }) => {
            if (node.type === NodeType.FOLDER && event.type !== "keydown") return;
            setTreeSelectKeys([node.id]);
            const demoId = (node as WorkbenchTreeNode).demoId;
            if (node.type !== NodeType.FILE || !demoId) return;
            const demo = demos.find((item) => item.id === demoId);
            if (demo) onChoose(demo);
          }}
        />
      </div>}
      {!hasTreeMatches && <Empty
        className="workbench-empty-state"
        preset="search"
        imageSize={48}
        title={text("No matching components", "没有匹配的组件")}
        description={text("Try another search term.", "请尝试其他搜索关键词。")}
      />}
    </nav>
  </div>;
}

function ControlsContent({
  selected,
  args,
  defaults,
  updateArg,
  showHeading = true,
}: {
  selected?: DemoRecord;
  args: Args;
  defaults: Args;
  updateArg: (name: string, value: unknown) => void;
  showHeading?: boolean;
}) {
  const controlCount = selected?.controls.filter((control) => control.kind !== "readonly").length || 0;
  return <>
    {showHeading && <div className="workbench-controls-heading"><strong>{text("Controls", "属性控件")}</strong><Tag className="workbench-count-tag" size="small">{controlCount}</Tag></div>}
    {selected?.controls.map((control) => <Control
      key={control.name}
      control={control}
      value={args[control.name]}
      defaultValue={defaults[control.name]}
      setValue={(value) => updateArg(control.name, value)}
      clear={() => updateArg(control.name, undefined)}
    />)}
    {!selected?.controls.length && <Empty
      className="workbench-empty-state workbench-no-controls"
      imageSize={48}
      title={text("No controls", "暂无属性控件")}
      description={text("Type the default demo parameter to generate controls.", "为默认导出的示例参数标注类型，即可生成控件。")}
    />}
  </>;
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
  const [navigationOpen, setNavigationOpen] = useState(false);
  const [controlsOpen, setControlsOpen] = useState(false);
  const [runtimeWarnings, setRuntimeWarnings] = useState<string[]>([]);
  const [error, setError] = useState("");
  const [copied, setCopied] = useState(false);
  const [sourceCopied, setSourceCopied] = useState(false);
  const frameRef = useRef<HTMLIFrameElement>(null);
  const isMobile = useMediaQuery("(max-width: 760px)");
  const { theme, resolved, setTheme } = useTheme();
  const selected = demos.find((demo) => demo.id === selectedId) || demos[0];
  const [treeData, setTreeData] = useState<TreeNode[]>(() => buildTreeData(demos));
  const [expandedKeys, setExpandedKeys] = useState<React.Key[]>(() =>
    buildTreeData(demos).filter((node) => node.type === NodeType.FOLDER).map((node) => node.id),
  );
  const [treeSelectKeys, setTreeSelectKeys] = useState<React.Key[]>(() =>
    initial ? [demoTreeKey(initial.id)] : [],
  );
  const searchNeedle = query.trim().toLowerCase();
  const filterTreeNode = useMemo(() => searchNeedle
    ? (node: TreeNode) => (node as WorkbenchTreeNode).searchText.includes(searchNeedle)
    : undefined, [searchNeedle]);
  const hasTreeMatches = !filterTreeNode || treeData.some(filterTreeNode);
  const searchExpandedKeys = useMemo(() => {
    if (!filterTreeNode) return expandedKeys;
    const keys = new Set<React.Key>();
    treeData.filter(filterTreeNode).forEach((node) => {
      let parent = node.parent;
      while (parent) {
        keys.add(parent.id);
        parent = parent.parent;
      }
    });
    return Array.from(keys);
  }, [expandedKeys, filterTreeNode, treeData]);
  const controlCount = selected?.controls.filter((control) => control.kind !== "readonly").length || 0;

  useEffect(() => {
    if (isMobile) return;
    setNavigationOpen(false);
    setControlsOpen(false);
  }, [isMobile]);

  useEffect(() => {
    if (selectedId) setTreeSelectKeys([demoTreeKey(selectedId)]);
  }, [selectedId]);

  useEffect(() => {
    const syncLocation = () => {
      const next = readLocation();
      if (next.id && demos.some((demo) => demo.id === next.id)) {
        if (next.id === selectedId && loadedId === next.id) {
          setArgs(applyLocationArgs(defaults, next));
        } else {
          setSourceOpen(false);
        }
        setSelectedId(next.id);
        setViewport(next.viewport);
      }
    };
    window.addEventListener("hashchange", syncLocation);
    window.addEventListener("popstate", syncLocation);
    return () => {
      window.removeEventListener("hashchange", syncLocation);
      window.removeEventListener("popstate", syncLocation);
    };
  }, [defaults, loadedId, selectedId]);
  useEffect(() => {
    if (!selected) return;
    setLoadedId("");
    setError("");
    setSource("");
    let active = true;
    Promise.all([selected.load(), selected.loadSource()]).then(([module, sourceModule]) => {
      if (!active) return;
      const metaArgs = serializableArgs(module.meta?.args);
      const nextDefaults = { ...controlDefaults(selected), ...metaArgs.args };
      const currentLocation = readLocation();
      const nextArgs = currentLocation.id === selected.id
        ? applyLocationArgs(nextDefaults, currentLocation)
        : nextDefaults;
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
        setError("");
        frameRef.current?.contentWindow?.postMessage({ type: "wake:theme", theme: resolved }, "*");
        frameRef.current?.contentWindow?.postMessage({ type: "wake:args", id: selected?.id, args }, "*");
      }
      if (event.data.type === "wake:error") setError(String(event.data.error || text("Demo failed", "示例运行失败")));
    };
    window.addEventListener("message", receive);
    return () => window.removeEventListener("message", receive);
  }, [selected?.id, args, resolved]);

  useEffect(() => {
    frameRef.current?.contentWindow?.postMessage({ type: "wake:theme", theme: resolved }, "*");
  }, [resolved]);

  const choose = (demo: DemoRecord) => {
    setNavigationOpen(false);
    if (demo.id === selected?.id) return;
    setSelectedId(demo.id);
    setArgs({});
    setDefaults({});
    setViewport("responsive");
    setSourceOpen(false);
    history.pushState(null, "", locationHash(demo.id, {}, {}, "responsive"));
  };
  const updateArg = (name: string, value: unknown) => {
    setError("");
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
  const copySource = () => copyText(source).then(() => {
    setSourceCopied(true);
    window.setTimeout(() => setSourceCopied(false), 1400);
    return false;
  }).catch((reason) => {
    setError(text("Copy source failed", "复制源码失败") + ": " + String(reason));
    return false;
  });
  const nextTheme: Theme = theme === "system" ? "light" : theme === "light" ? "dark" : "system";
  const cycleTheme = () => setTheme(nextTheme);
  const resetTooltip = text("Reset controls", "重置属性");
  const resetActionLabel = text("Restore all controls to their defaults", "将所有属性恢复为默认值");
  const themeTooltip = theme === "system"
    ? text(`System theme (currently ${themeLabel(resolved)})`, `跟随系统（当前显示：${themeLabel(resolved)}）`)
    : text(`Theme: ${themeLabel(theme)}`, `主题：${themeLabel(theme)}`);
  const themeActionLabel = theme === "system"
    ? text(
      `Theme follows the system (currently ${themeLabel(resolved)}); switch to Light`,
      `当前设置：跟随系统（当前显示：${themeLabel(resolved)}）；切换到浅色`,
    )
    : text(
      `Current theme: ${themeLabel(theme)}; switch to ${themeLabel(nextTheme)}`,
      `当前主题：${themeLabel(theme)}；切换到${themeLabel(nextTheme)}`,
    );
  const copyTooltip = copied ? text("Link copied", "链接已复制") : text("Copy link", "复制链接");
  const copyActionLabel = copied
    ? text("Link copied", "链接已复制")
    : text("Copy the current page link", "复制当前页面链接");
  const ThemeIcon = theme === "light" ? Sun : theme === "dark" ? Moon : Monitor;
  const sourceActionLabel = text("View source code", "查看源码");
  const frameUrl = selected ? siteConfig.basePath + "?__wake_demo=" + encodeURIComponent(selected.id) : "";
  const viewportLabel = viewport === "mobile" ? "390px" : viewport === "tablet" ? "768px" : "100%";

  if (!demos.length) return <main className="workbench-empty">
    <Empty
      className="workbench-empty-state"
      title={text("No component demos found", "没有找到组件示例")}
      description={text("Add a *.demo.tsx file under the configured docs source directory.", "请在配置的文档目录中添加 *.demo.tsx 文件。")}
    />
  </main>;

  return <div className="workbench-shell">
    <aside className="workbench-sidebar">
      <div className="workbench-brand"><strong>{siteConfig.title}</strong><span>{text("Components", "组件")}</span></div>
      <DemoCatalog
        className="workbench-desktop-catalog"
        viewportClassName="workbench-tree-viewport"
        active={!isMobile}
        query={query}
        setQuery={setQuery}
        treeData={treeData}
        setTreeData={setTreeData}
        expandedKeys={expandedKeys}
        setExpandedKeys={setExpandedKeys}
        treeSelectKeys={treeSelectKeys}
        setTreeSelectKeys={setTreeSelectKeys}
        searchNeedle={searchNeedle}
        filterTreeNode={filterTreeNode}
        searchExpandedKeys={searchExpandedKeys}
        hasTreeMatches={hasTreeMatches}
        onChoose={choose}
      />
    </aside>
    <main className="workbench-main">
      <header className="workbench-toolbar">
        <div className="workbench-toolbar-heading">
          <Button
            className="workbench-mobile-trigger workbench-mobile-navigation-trigger"
            appearance="text"
            size="middle"
            shape="circle"
            type="button"
            icon={<Menu size={18} aria-hidden="true" />}
            aria-label={text("Open component list", "打开组件列表")}
            aria-haspopup="dialog"
            aria-controls="workbench-navigation-drawer"
            aria-expanded={navigationOpen}
            onClick={() => setNavigationOpen(true)}
          />
          <div className="workbench-toolbar-title"><strong>{selected?.component}</strong><span>/ {selected?.title}</span></div>
          <Button
            className="workbench-mobile-trigger workbench-mobile-controls-trigger"
            appearance="text"
            size="middle"
            shape="circle"
            type="button"
            icon={<SlidersHorizontal size={18} aria-hidden="true" />}
            aria-label={text(`Open ${controlCount} controls`, `打开 ${controlCount} 个属性控件`)}
            aria-haspopup="dialog"
            aria-controls="workbench-controls-drawer"
            aria-expanded={controlsOpen}
            onClick={() => setControlsOpen(true)}
          />
        </div>
        <div className="workbench-toolbar-actions">
          <Segmented
            className="workbench-viewport-selector"
            aria-label={text("Preview viewport", "预览视口")}
            value={viewport}
            options={[
              { value: "responsive", label: text("Wide", "宽屏") },
              { value: "tablet", label: text("Tablet", "平板") },
              { value: "mobile", label: text("Mobile", "手机") },
            ]}
            onChange={(next) => setViewport(next as Viewport)}
          />
          <div className="workbench-action-group" role="group" aria-label={text("Workbench actions", "工作台操作")}>
            <Tooltip title={sourceActionLabel} placement="bottom">
              <Button
                className="workbench-action-button workbench-action-source"
                appearance="text"
                size="middle"
                shape="circle"
                type="button"
                icon={<Code2 size={16} aria-hidden="true" />}
                aria-label={sourceActionLabel}
                aria-haspopup="dialog"
                aria-expanded={sourceOpen}
                onClick={() => setSourceOpen(true)}
              />
            </Tooltip>
            <Tooltip title={resetTooltip} placement="bottom">
              <Button
                className="workbench-action-button workbench-action-reset"
                appearance="text"
                size="middle"
                shape="circle"
                type="button"
                icon={<RotateCcw size={16} aria-hidden="true" />}
                aria-label={resetActionLabel}
                onClick={() => {
                  setError("");
                  setArgs(defaults);
                }}
              />
            </Tooltip>
            <Tooltip title={themeTooltip} placement="bottom">
              <Button
                className="workbench-action-button workbench-action-theme"
                appearance="text"
                size="middle"
                shape="circle"
                type="button"
                icon={<ThemeIcon size={16} aria-hidden="true" />}
                aria-label={themeActionLabel}
                onClick={cycleTheme}
              />
            </Tooltip>
            <Tooltip title={copyTooltip} placement="bottom">
              <Button
                className="workbench-action-button workbench-action-copy"
                appearance="text"
                size="middle"
                shape="circle"
                type="button"
                icon={copied
                  ? <Check size={16} aria-hidden="true" />
                  : <Copy size={16} aria-hidden="true" />}
                aria-label={copyActionLabel}
                onClick={copyLink}
              />
            </Tooltip>
            <span className="sr-only" role="status" aria-live="polite">{copied ? copyActionLabel : ""}</span>
          </div>
        </div>
      </header>
      <div className="workbench-canvas">
        <div className={"workbench-viewport workbench-viewport-" + viewport} style={{ width: viewportLabel }}>
          {selected && <iframe key={selected.id} ref={frameRef} title={selected.title} src={frameUrl} sandbox="allow-scripts allow-same-origin" />}
        </div>
      </div>
      {Boolean(error || selected?.warnings.length || runtimeWarnings.length) && <div className="workbench-diagnostics" role="status">
        {error && <Alert className="workbench-diagnostics-alert" type="error" title={text("Runtime error", "运行错误")} showIcon>{error}</Alert>}
        {selected?.warnings.map((warning) => <Alert className="workbench-diagnostics-alert" type="warning" title={text("Warning", "警告")} showIcon key={warning}>{warning}</Alert>)}
        {runtimeWarnings.map((warning) => <Alert className="workbench-diagnostics-alert" type="warning" title={text("Warning", "警告")} showIcon key={warning}>{warning}</Alert>)}
      </div>}
      <Dialog
        className="workbench-source-dialog"
        open={sourceOpen}
        onOpenChange={(open) => {
          setSourceOpen(open);
          if (!open) setSourceCopied(false);
        }}
        title={text("Source code", "源码") + (selected ? ` · ${selected.title}` : "")}
        maskClosable
        i18n={{
          cancelText: text("Close", "关闭"),
          confirmText: sourceCopied ? text("Copied", "已复制") : text("Copy source", "复制源码"),
        }}
        onConfirm={copySource}
      >
        <div className="workbench-source-dialog-body">
          {selected && <div className="workbench-source-path">
            <Code2 size={14} aria-hidden="true" />
            <code>{selected.id}</code>
          </div>}
          {source
            ? <pre tabIndex={0}><code>{source}</code></pre>
            : <Empty
              className="workbench-empty-state"
              imageSize={48}
              title={text("Loading source code", "正在加载源码")}
            />}
          <span className="sr-only" role="status" aria-live="polite">{sourceCopied ? text("Source copied", "源码已复制") : ""}</span>
        </div>
      </Dialog>
    </main>
    <aside className="workbench-controls">
      <ControlsContent selected={selected} args={args} defaults={defaults} updateArg={updateArg} />
    </aside>
    {isMobile && <>
      <Drawer
        id="workbench-navigation-drawer"
        className="workbench-mobile-drawer workbench-mobile-navigation-drawer"
        open={navigationOpen}
        onOpenChange={setNavigationOpen}
        placement="left"
        size="small"
        title={text("Components", "组件列表")}
        closeLabel={text("Close component list", "关闭组件列表")}
      >
        <DemoCatalog
          className="workbench-mobile-catalog"
          viewportClassName="workbench-mobile-tree-viewport"
          active={navigationOpen}
          query={query}
          setQuery={setQuery}
          treeData={treeData}
          setTreeData={setTreeData}
          expandedKeys={expandedKeys}
          setExpandedKeys={setExpandedKeys}
          treeSelectKeys={treeSelectKeys}
          setTreeSelectKeys={setTreeSelectKeys}
          searchNeedle={searchNeedle}
          filterTreeNode={filterTreeNode}
          searchExpandedKeys={searchExpandedKeys}
          hasTreeMatches={hasTreeMatches}
          onChoose={choose}
        />
      </Drawer>
      <Drawer
        id="workbench-controls-drawer"
        className="workbench-mobile-drawer workbench-mobile-controls-drawer"
        open={controlsOpen}
        onOpenChange={setControlsOpen}
        placement="right"
        size="medium"
        title={<span className="workbench-mobile-drawer-title">{text("Controls", "属性控件")}<Tag className="workbench-count-tag" size="small">{controlCount}</Tag></span>}
        closeLabel={text("Close controls", "关闭属性控件")}
      >
        <ControlsContent selected={selected} args={args} defaults={defaults} updateArg={updateArg} showHeading={false} />
      </Drawer>
    </>}
  </div>;
}
