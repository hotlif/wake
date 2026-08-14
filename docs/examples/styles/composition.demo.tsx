import React, { useState } from "react";
import { css, cx } from "@crab-dev/css";

export const meta = {
  title: "条件组合",
  description: "切换状态，观察 cx 保持稳定类名顺序。",
  order: 20,
  viewport: "responsive",
  background: "surface",
  padding: "lg",
};

const layout = css`
  display: grid;
  justify-items: start;
  gap: 14px;
`;

const item = css`
  min-width: 180px;
  padding: 12px 16px;
  color: #334155;
  border: 1px solid #cbd5e1;
  border-radius: 10px;
  background: #fff;
`;

const selected = css`
  color: #5b21b6;
  border-color: #7c3aed;
  outline: 3px solid #ede9fe;
`;

const compact = css`
  padding-block: 6px;
`;

const controls = css`
  display: flex;
  flex-wrap: wrap;
  gap: 12px;

  & > label { display: flex; align-items: center; gap: 6px; }
`;

export default function CompositionDemo() {
  const [active, setActive] = useState(true);
  const [small, setSmall] = useState(false);

  return (
    <div className={layout}>
      <div className={cx(item, active && selected, [small && compact])}>可组合项目</div>
      <div className={controls}>
        <label><input type="checkbox" checked={active} onChange={(event) => setActive(event.target.checked)} />选中</label>
        <label><input type="checkbox" checked={small} onChange={(event) => setSmall(event.target.checked)} />紧凑</label>
      </div>
    </div>
  );
}
