import React, { useState } from "react";
import DocumentedButton from "../crab-button";

export const meta = {
  title: "基础交互",
  description: "点击 npm 发布的 Crab UI Button，观察本地状态更新。",
  height: "auto",
  viewport: "responsive",
  background: "surface",
  padding: "lg",
  isolation: "iframe",
};

export default function BasicButtonDemo() {
  const [count, setCount] = useState(0);

  return (
    <div style={{ display: "grid", justifyItems: "start", gap: 14 }}>
      <p aria-live="polite" style={{ margin: 0 }}>
        已完成 {count} 次构建
      </p>
      <DocumentedButton showArrow onClick={() => setCount((value) => value + 1)}>
        开始构建
      </DocumentedButton>
    </div>
  );
}
