import { createRoot } from "react-dom/client";
import _ from "lodash";
import { Card, StatsList, Counter, Badge } from "./App.js";
import { slugify, pickTop, buildStats, summarize } from "./utils.js";
import type { CardProps, Theme } from "./types.js";

// web 版 React 19 入口：真实 JSX + react-dom/client 渲染真实 DOM。
const theme: Theme = "dark";

const cardProps: CardProps = {
  title: "hello wake",
  body: "由 wake 打包 —— TypeScript + React 19 + lodash + JSX，全程无插件。",
  tags: ["typescript", "react", "jsx", "lodash", "enum"],
  badge: Badge.Sale,
};

const numbers: number[] = [5, 2, 9, 1, 7, 3, 9, 2];

const meta =
  summarize(numbers) +
  " ｜ slug=" +
  slugify(cardProps.title) +
  " ｜ top3=" +
  pickTop(numbers, 3).join(", ") +
  " ｜ chunks=" +
  JSON.stringify(_.chunk(numbers, 3));

const app = (
  <main className="app" data-theme={theme}>
    <h1>wake · TypeScript + React 19 + lodash（JSX + Live Reload）</h1>
    <Card {...cardProps} />
    <StatsList stats={buildStats(numbers)} />
    <Counter />
    <p className="meta">{meta}</p>
  </main>
);

const container = document.getElementById("root");
const root = createRoot(container!);
root.render(app);

console.log("[wake] React 19 + JSX 已挂载到 #root，DOM 已生成。");
