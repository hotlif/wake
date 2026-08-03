import React from "react";
import Alert from "@crab-dev/rc-alert";
import Button from "@crab-dev/rc-button";
import Card from "@crab-dev/rc-card";
import Prose from "@crab-dev/rc-prose";
import Tag from "@crab-dev/rc-tag";


export function HomeHero() {
  return <section className="wake-home-hero">
    <div className="wake-hero-orb wake-hero-orb-one" />
    <div className="wake-hero-orb wake-hero-orb-two" />
    <div className="wake-home-copy">
      <Tag className="wake-kicker" color="primary" bordered={false}>
        <i /> React 19 · 应用构建 · 组件文档
      </Tag>
      <h1>构建 React 应用，<br /><em>也构建组件文档</em></h1>
      <p>Wake 把开发服务器、生产打包与完整 MDX 文档放进同一套 Rust 工具链。第一次使用时，从学习路线开始，亲手完成一个可运行的应用和文档站。</p>
      <div className="wake-home-actions">
        <Button className="wake-action-primary" href="./getting-started/learning-path" appearance="primary" size="large" iconAfter={<span aria-hidden="true">→</span>}>
          按学习路线开始
        </Button>
        <Button className="wake-action-secondary" href="./getting-started/first-docs-site" appearance="subtle" size="large">
          创建第一个文档站
        </Button>
      </div>
      <div className="wake-home-meta" aria-label="Wake 关键能力"><span>React 19+</span><span>完整 MDX</span><span>隔离 Demo</span><span>Props API</span></div>
    </div>
    <div className="wake-terminal" aria-label="Wake 文档构建输出示例">
      <div className="wake-terminal-bar"><span /><span /><span /><small>wake docs build .</small></div>
      <pre><code><b>⚡ wake v0.1.0</b><br /><br />  ✓ 文档构建成功  ·  15 routes<br />    → docs-dist/index.html<br />    → docs-dist/404.html</code></pre>
    </div>
  </section>;
}

export function HomeFeatures() {
  const features = [
    ["保存即可看到结果", "开发服务器监听源码、样式和文档变化，只重新处理受到影响的模块。", "01", "primary"],
    ["以 React 19 为基线", "直接使用项目中的 React、React DOM 和 automatic JSX runtime，不附带另一套运行时。", "02", "success"],
    ["开发完成即可生产构建", "代码分割、Tree Shaking、CSS 抽取、资源哈希和静态 HTML 使用同一套模块图。", "03", "warning"],
    ["组件文档不是附属品", "MDX、交互 Demo、Props API、搜索和静态路由都由 Wake 编译并持续更新。", "04", "default"],
  ];
  return <section className="wake-home-section" aria-labelledby="wake-features-title">
    <header className="wake-home-section-heading"><Tag color="primary" bordered={false}>核心能力</Tag><h2 id="wake-features-title">从第一次保存，到可部署产物</h2><p>学习时只需要掌握两组命令；Wake 负责让应用代码、文档示例和生产输出保持一致。</p></header>
    <div className="wake-feature-grid">
      {features.map(([title, body, number, color]) => <Card key={number} className="wake-feature-card" variant="outlined" size="large">
        <Tag className="wake-feature-index" color={color} size="small" bordered={false}>{number}</Tag>
        <h3>{title}</h3><p>{body}</p>
      </Card>)}
    </div>
  </section>;
}

export function HomePaths() {
  const paths = [
    {
      tag: "路线 A",
      title: "先构建一个 React 19 应用",
      body: "适合第一次使用 Wake，或准备把现有 React 项目迁移到 Wake 的开发者。",
      points: ["创建入口、样式和最小配置", "启动开发服务器并验证更新", "生成带资源哈希的 dist"],
      href: "./getting-started/quick-start",
      action: "进入快速开始",
    },
    {
      tag: "路线 B",
      title: "先创建一个组件文档站",
      body: "适合维护设计系统、组件库或内部 UI 平台，希望直接体验 MDX、Demo 和 Props API 的开发者。",
      points: ["创建中文 MDX 页面和导航", "运行隔离的 React 19 Demo", "生成可搜索的 Props 文档"],
      href: "./getting-started/first-docs-site",
      action: "创建第一个文档站",
    },
  ];
  return <section className="wake-home-section wake-home-paths" aria-labelledby="wake-paths-title">
    <header className="wake-home-section-heading"><Tag color="success" bordered={false}>从这里开始</Tag><h2 id="wake-paths-title">选择与你当前目标一致的路线</h2><p>不需要先读完全部参考资料。完成一条路线后，再回到学习路线补齐开发、生产和排错知识。</p></header>
    <div className="wake-path-grid">
      {paths.map((path) => <Card key={path.tag} className="wake-path-card" variant="outlined" size="large">
        <Tag color="primary" size="small" bordered={false}>{path.tag}</Tag>
        <h3>{path.title}</h3>
        <p>{path.body}</p>
        <ul>{path.points.map((point) => <li key={point}>{point}</li>)}</ul>
        <Button className="wake-path-action" href={path.href} appearance="primary" size="middle" iconAfter={<span aria-hidden="true">→</span>}>{path.action}</Button>
      </Card>)}
    </div>
    <nav className="wake-home-shortcuts" aria-label="熟练用户快捷入口"><strong>已经开始使用 Wake？</strong><a href="./guide/development">开发与 HMR</a><a href="./guide/production">生产构建</a><a href="./reference/cli">CLI 参考</a><a href="./reference/troubleshooting">故障排查</a></nav>
  </section>;
}

export function Callout({ title, tone = "info", children }: { title: string; tone?: "info" | "warning" | "success"; children: React.ReactNode }) {
  return <Alert className={`wake-callout wake-callout-${tone}`} type={tone} title={title} showIcon>
    <Prose className="wake-callout-prose" size="sm">{children}</Prose>
  </Alert>;
}

export function CrabComponentShowcase() {
  return <section className="wake-component-showcase" aria-label="Crab UI 组件示例">
    <Card
      className="wake-component-card"
      variant="outlined"
      size="large"
      title={<span className="wake-component-title">项目级组件 <Tag color="success" size="small">npm</Tag></span>}
      extra={<Tag color="primary" bordered={false}>React 19</Tag>}
      actions={[
        <Button key="guide" href="../getting-started/quick-start" appearance="subtle" size="small">阅读快速开始</Button>,
        <Button key="cli" href="../reference/cli" appearance="primary" size="small">查看 CLI</Button>,
      ]}
    >
      <Prose size="sm">
        <p>这块内容由已发布的 <code>@crab-dev/rc-card</code>、<code>rc-tag</code>、<code>rc-button</code> 与 <code>rc-prose</code> 共同渲染。</p>
      </Prose>
    </Card>
    <Alert type="success" title="边界清晰" showIcon>
      Wake Docs 负责 MDX、路由和构建，Crab UI 只属于当前文档项目的展示层。
    </Alert>
  </section>;
}
