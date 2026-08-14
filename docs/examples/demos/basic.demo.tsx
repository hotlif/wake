import React, { useState } from "react";
import { css } from "@crab-dev/css";

const layout = css`
  display: grid;
  justify-items: start;
  gap: 14px;
`;

const status = css`
  margin: 0;
  color: #334155;
`;

const action = css`
  padding: 8px 14px;
  color: #fff;
  border: 0;
  border-radius: 8px;
  background: #2563eb;
  cursor: pointer;

  &:hover { background: #1d4ed8; }
  &:focus-visible { outline: 3px solid #93c5fd; outline-offset: 2px; }
`;

export const meta = {
  title: "基础交互",
  description: "点击原生按钮，观察文档 Demo 中的本地状态更新。",
  height: "auto",
  viewport: "responsive",
  background: "surface",
  padding: "lg",
  isolation: "iframe",
};

export default function BasicInteractionDemo() {
  const [count, setCount] = useState(0);

  return (
    <div className={layout}>
      <p className={status} aria-live="polite">
        已完成 {count} 次构建
      </p>
      <button className={action} type="button" onClick={() => setCount((value) => value + 1)}>
        开始构建
      </button>
    </div>
  );
}
