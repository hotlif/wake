import React, { useState } from "react";
import { assignVars, createVar, css } from "@crab-dev/css";

export const meta = {
  title: "动态变量",
  description: "React 只更新 CSS 自定义属性，类名和样式表保持稳定。",
  order: 30,
  viewport: "responsive",
  background: "surface",
  padding: "lg",
};

const accent = createVar("meter-accent");
const progress = createVar("meter-progress");

const layout = css`
  display: grid;
  gap: 12px;
  width: min(100%, 420px);
`;

const track = css`
  height: 14px;
  overflow: hidden;
  border-radius: 999px;
  background: #e2e8f0;

  &::before {
    display: block;
    width: 100%;
    height: 100%;
    border-radius: inherit;
    background: ${accent};
    content: "";
    transform: scaleX(${progress});
    transform-origin: left;
    transition: transform 140ms ease, background 140ms ease;
  }
`;

const controls = css`
  display: grid;
  grid-template-columns: 1fr auto;
  gap: 8px 12px;
  align-items: center;

  & > label { color: #475569; }
`;

export default function DynamicValuesDemo() {
  const [value, setValue] = useState(64);
  const [color, setColor] = useState("#2563eb");

  return (
    <div className={layout} style={assignVars({ [accent]: color, [progress]: value / 100 })}>
      <div className={track} aria-label={`进度 ${value}%`} role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={value} />
      <div className={controls}>
        <label htmlFor="demo-progress">进度：{value}%</label>
        <input id="demo-color" type="color" aria-label="进度颜色" value={color} onChange={(event) => setColor(event.target.value)} />
        <input id="demo-progress" type="range" min="0" max="100" value={value} onChange={(event) => setValue(Number(event.target.value))} />
      </div>
    </div>
  );
}
