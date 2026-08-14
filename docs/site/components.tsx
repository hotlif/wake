import React from "react";

export type VisualName = "home" | "hmr" | "build" | "dynamic-css";
export type FeatureIconName =
  | "app"
  | "style"
  | "docs"
  | "search"
  | "page"
  | "navigation"
  | "demo"
  | "api"
  | "preview"
  | "deploy"
  | "scope"
  | "compose"
  | "variable"
  | "animation"
  | "global"
  | "boundary";

type Children = { children: React.ReactNode };

function unwrapMdxParagraphs(children: React.ReactNode) {
  return React.Children.toArray(children).flatMap((child) => {
    if (!React.isValidElement<{ children?: React.ReactNode }>(child) || child.type !== "p") return [child];
    return React.Children.toArray(child.props.children);
  });
}

function Arrow({ d }: { d: string }) {
  return <path className="diagram-arrow" d={d} />;
}

function Node({ x, y, width, label, accent = false }: { x: number; y: number; width: number; label: string; accent?: boolean }) {
  return <g className={accent ? "diagram-node diagram-node-accent" : "diagram-node"}>
    <rect x={x} y={y} width={width} height="38" rx="4" />
    <text x={x + width / 2} y={y + 24} textAnchor="middle">{label}</text>
  </g>;
}

function TechnicalVisual({ name, alt, decorative = false }: { name: VisualName; alt?: string; decorative?: boolean }) {
  let content: React.ReactNode;

  switch (name) {
    case "home":
      content = <>
        <Arrow d="M138 57H236" /><Arrow d="M138 143H236" /><Arrow d="M404 57H502" /><Arrow d="M404 143H502" />
        <Node x={34} y={38} width={104} label="React" />
        <Node x={34} y={124} width={104} label="TypeScript" />
        <Node x={236} y={76} width={168} label="Wake" accent />
        <Node x={502} y={38} width={104} label="CSS" />
        <Node x={502} y={124} width={104} label="Assets" />
      </>;
      break;
    case "hmr":
      content = <>
        <Node x={28} y={76} width={136} label="File change" />
        <Node x={252} y={76} width={136} label="Module update" accent />
        <Node x={476} y={76} width={136} label="Browser state" />
        <Arrow d="M164 95H252" /><Arrow d="M388 95H476" /><Arrow d="M544 122C544 171 96 171 96 122" />
      </>;
      break;
    case "build":
      content = <>
        <Node x={24} y={76} width={124} label="Module graph" />
        <Node x={258} y={76} width={124} label="Optimize" accent />
        <Node x={492} y={18} width={124} label="JavaScript" />
        <Node x={492} y={66} width={124} label="CSS" />
        <Node x={492} y={114} width={124} label="Assets" />
        <Node x={492} y={162} width={124} label="Manifest" />
        <Arrow d="M148 95H258" /><Arrow d="M382 95H440V37H492" /><Arrow d="M440 95H492" /><Arrow d="M440 95V133H492" /><Arrow d="M440 95V181H492" />
      </>;
      break;
    case "dynamic-css":
      content = <>
        <Node x={24} y={76} width={136} label="React value" />
        <Node x={252} y={76} width={136} label="CSS variable" accent />
        <Node x={480} y={76} width={136} label="Static rule" />
        <Arrow d="M160 95H252" /><Arrow d="M388 95H480" />
      </>;
      break;
  }

  return <svg
    className={`technical-visual technical-visual-${name}`}
    viewBox="0 0 640 200"
    role={!decorative && alt ? "img" : undefined}
    aria-hidden={decorative || !alt ? true : undefined}
    aria-label={!decorative ? alt : undefined}
  >
    {!decorative && alt && <title>{alt}</title>}
    {content}
  </svg>;
}

function Actions({ primaryHref, primaryLabel, secondaryHref, secondaryLabel }: {
  primaryHref: string;
  primaryLabel: string;
  secondaryHref?: string;
  secondaryLabel?: string;
}) {
  return <div className="wake-actions">
    <a className="wake-button wake-button-primary" href={primaryHref}>{primaryLabel}<span aria-hidden="true">→</span></a>
    {secondaryHref && secondaryLabel && <a className="wake-button" href={secondaryHref}>{secondaryLabel}</a>}
  </div>;
}

function FeatureIcon({ name }: { name: FeatureIconName }) {
  let paths: React.ReactNode;
  switch (name) {
    case "app": paths = <><rect x="3" y="4" width="18" height="16" rx="2" /><path d="M3 9h18M8 9v11" /></>; break;
    case "style": paths = <><path d="M12 3c4 4 7 7 7 11a7 7 0 0 1-14 0c0-4 3-7 7-11Z" /><path d="M9 17c1.4.8 3.3.8 5 0" /></>; break;
    case "docs": paths = <><path d="M5 3h10l4 4v14H5z" /><path d="M15 3v5h4M8 12h8M8 16h6" /></>; break;
    case "search": paths = <><circle cx="10.5" cy="10.5" r="6.5" /><path d="m16 16 5 5" /></>; break;
    case "page": paths = <><path d="M5 3h10l4 4v14H5z" /><path d="M15 3v5h4M8 12h8M8 16h8" /></>; break;
    case "navigation": paths = <><path d="M5 5h14M5 12h9M5 19h14" /><circle cx="18" cy="12" r="2" /></>; break;
    case "demo": paths = <><rect x="3" y="4" width="18" height="16" rx="2" /><path d="m10 9 5 3-5 3z" /></>; break;
    case "api": paths = <><path d="M8 5 3 12l5 7M16 5l5 7-5 7M14 3l-4 18" /></>; break;
    case "preview": paths = <><path d="M2.5 12s3.5-6 9.5-6 9.5 6 9.5 6-3.5 6-9.5 6-9.5-6-9.5-6Z" /><circle cx="12" cy="12" r="2.5" /></>; break;
    case "deploy": paths = <><path d="M12 3v12M7 8l5-5 5 5" /><path d="M4 14v6h16v-6" /></>; break;
    case "scope": paths = <><rect x="4" y="4" width="16" height="16" rx="3" /><path d="M8 9h8M8 13h5" /></>; break;
    case "compose": paths = <><rect x="3" y="5" width="10" height="10" rx="2" /><rect x="11" y="9" width="10" height="10" rx="2" /></>; break;
    case "variable": paths = <><path d="M4 7h16M4 17h16" /><circle cx="9" cy="7" r="2" /><circle cx="15" cy="17" r="2" /></>; break;
    case "animation": paths = <><path d="M20 11a8 8 0 1 1-3-6.2" /><path d="M17 2v4h4" /><path d="M12 8v5l3 2" /></>; break;
    case "global": paths = <><circle cx="12" cy="12" r="9" /><path d="M3 12h18M12 3c3 3 3 15 0 18M12 3c-3 3-3 15 0 18" /></>; break;
    case "boundary": paths = <><path d="M12 3 5 6v5c0 5 2.8 8.2 7 10 4.2-1.8 7-5 7-10V6z" /><path d="m9 12 2 2 4-5" /></>; break;
  }
  return <svg className="wake-feature-icon" viewBox="0 0 24 24" aria-hidden="true">{paths}</svg>;
}

export function PageActions(props: {
  primaryHref: string;
  primaryLabel: string;
  secondaryHref?: string;
  secondaryLabel?: string;
}) {
  return <div className="wake-page-actions"><Actions {...props} /></div>;
}

export function FeatureGrid({ children }: Children) {
  return <div className="wake-feature-grid">{unwrapMdxParagraphs(children)}</div>;
}

export function FeatureCard({ icon, href, title, description, label }: {
  icon: FeatureIconName;
  href: string;
  title: string;
  description: string;
  label?: string;
}) {
  return <a className="wake-feature-card" href={href}>
    <span className="wake-feature-icon-wrap"><FeatureIcon name={icon} /></span>
    <span className="wake-feature-copy">
      {label && <small>{label}</small>}
      <strong>{title}</strong>
      <p>{description}</p>
    </span>
    <i aria-hidden="true">↗</i>
  </a>;
}

export function HomeLead({
  title,
  description,
  status,
  primaryHref,
  primaryLabel,
  secondaryHref,
  secondaryLabel,
  children,
}: Children & {
  title: string;
  description: string;
  status?: string;
  primaryHref: string;
  primaryLabel: string;
  secondaryHref?: string;
  secondaryLabel?: string;
}) {
  return <section className="wake-home-lead">
    <div className="wake-home-copy">
      {status && <small className="wake-home-status">{status}</small>}
      <h1>{title}</h1>
      <p className="wake-home-description">{description}</p>
      <div className="wake-home-message">{children}</div>
      <Actions primaryHref={primaryHref} primaryLabel={primaryLabel} secondaryHref={secondaryHref} secondaryLabel={secondaryLabel} />
    </div>
    <div className="wake-home-diagram"><TechnicalVisual name="home" decorative /></div>
  </section>;
}

export function OverviewLead({
  primaryHref,
  primaryLabel,
  secondaryHref,
  secondaryLabel,
  children,
}: Children & {
  primaryHref: string;
  primaryLabel: string;
  secondaryHref?: string;
  secondaryLabel?: string;
}) {
  return <section className="wake-overview-lead">
    <div>{children}</div>
    <Actions primaryHref={primaryHref} primaryLabel={primaryLabel} secondaryHref={secondaryHref} secondaryLabel={secondaryLabel} />
  </section>;
}

export function TaskGrid({ children }: Children) {
  return <div className="wake-task-grid">{unwrapMdxParagraphs(children)}</div>;
}

export function TaskCard({ href, title, description, kicker }: { href: string; title: string; description: string; kicker?: string }) {
  return <a className="wake-task-card" href={href}>
    <span className="wake-task-copy">{kicker && <small>{kicker}</small>}<strong>{title}</strong><p>{description}</p></span>
    <i aria-hidden="true">→</i>
  </a>;
}

export function StepFlow({ children }: Children) {
  return <ol className="wake-step-flow">{unwrapMdxParagraphs(children)}</ol>;
}

export function Step({ number, title, children }: Children & { number: string; title: string }) {
  return <li><span>{number}</span><div><strong>{title}</strong><div>{children}</div></div></li>;
}

export function ResultPanel({ title, label = "EXPECTED", children }: Children & { title: string; label?: string }) {
  return <aside className="wake-result-panel" aria-label={`${label}: ${title}`}>
    <div><small>{label}</small><strong>{title}</strong></div>
    <div>{children}</div>
  </aside>;
}

export function VisualFigure({ visual, alt, caption }: { visual: Exclude<VisualName, "home">; alt: string; caption?: string }) {
  return <figure className="wake-visual-figure"><TechnicalVisual name={visual} alt={alt} />{caption && <figcaption>{caption}</figcaption>}</figure>;
}

export function CompareCards({ children }: Children) {
  return <div className="wake-compare-cards">{unwrapMdxParagraphs(children)}</div>;
}

export function CompareCard({ title, tone = "neutral", children }: Children & { title: string; tone?: "neutral" | "positive" | "warning" }) {
  return <section className={`wake-compare-card wake-compare-${tone}`}><strong>{title}</strong><div>{children}</div></section>;
}

export function NextActions({ children }: Children) {
  return <nav className="wake-next-actions" aria-label="下一步">{unwrapMdxParagraphs(children)}</nav>;
}

export function NextLink({ href, title, description }: { href: string; title: string; description: string }) {
  return <a className="wake-next-link" href={href}><span><strong>{title}</strong><small>{description}</small></span><i aria-hidden="true">→</i></a>;
}

export function MetricStrip({ children }: Children) {
  return <dl className="wake-metric-strip">{unwrapMdxParagraphs(children)}</dl>;
}

export function Metric({ value, label }: { value: string; label: string }) {
  return <div><dt>{value}</dt><dd>{label}</dd></div>;
}

export function Callout({
  title,
  tone = "info",
  children,
}: Children & {
  title: string;
  tone?: "info" | "warning" | "success";
}) {
  return <aside className={`wake-callout wake-callout-${tone}`} role={tone === "warning" ? "alert" : "note"}>
    <strong>{title}</strong>
    <div className="wake-callout-prose">{children}</div>
  </aside>;
}
