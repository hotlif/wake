import React from "react";
import { globalStyle } from "@crab-dev/css";

export const meta = {
  title: "显式全局规则",
  description: "globalStyle 在模块顶层声明，并用明确选择器限定示例作用域。",
  order: 50,
  viewport: "responsive",
  background: "surface",
  padding: "lg",
};

globalStyle`
  .crab-global-demo,
  .crab-global-demo * {
    box-sizing: border-box;
  }

  .crab-global-demo {
    width: min(100%, 420px);
    padding: 18px;
    color: #164e63;
    border-radius: 14px;
    background: #ecfeff;
    font-family: ui-sans-serif, system-ui, sans-serif;
  }

  .crab-global-demo h3 { margin: 0 0 8px; }
  .crab-global-demo p { margin: 0; line-height: 1.6; }
`;

export default function GlobalStylesDemo() {
  return (
    <section className="crab-global-demo">
      <h3>明确的全局边界</h3>
      <p>规则来自模块顶层的 globalStyle，并由 Demo iframe 隔离。</p>
    </section>
  );
}
