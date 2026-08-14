import React from "react";
import { css } from "@crab-dev/css";

export const meta = {
  title: "局部样式",
  description: "观察局部类名、嵌套选择器、hover 与窄屏规则。",
  order: 10,
  viewport: "responsive",
  background: "muted",
  padding: "lg",
};

const card = css`
  display: grid;
  gap: 10px;
  width: min(100%, 420px);
  padding: 18px;
  color: #1e293b;
  border: 1px solid #cbd5e1;
  border-radius: 14px;
  background: #fff;
  transition: border-color 140ms ease, transform 140ms ease;

  &:hover {
    border-color: #7c3aed;
    transform: translateY(-2px);
  }

  & > h3 { margin: 0; }
  & > p { margin: 0; color: #64748b; }

  @media (width < 640px) {
    padding: 13px;
  }
`;

export default function LocalStylesDemo() {
  return (
    <article className={card}>
      <h3>构建产物</h3>
      <p>将鼠标移到卡片上，再切换到手机视口。</p>
    </article>
  );
}
