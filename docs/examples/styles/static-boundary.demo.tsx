import React from "react";
import { css } from "@crab-dev/css";

export const meta = {
  title: "可证明的静态值",
  description: "顶层不可变常量在构建期求值，不进入浏览器运行时。",
  order: 60,
  viewport: "responsive",
  background: "muted",
  padding: "lg",
};

const unit = 4;
const gap = `${unit * 2}px`;
const accent = "#0f766e";

const tokens = css`
  display: grid;
  gap: ${gap};
  width: min(100%, 420px);
  padding: ${unit * 4}px;
  color: ${accent};
  border: 1px solid #99f6e4;
  border-radius: 12px;
  background: #f0fdfa;

  & > strong, & > span { display: block; }
`;

export default function StaticBoundaryDemo() {
  return (
    <div className={tokens}>
      <strong>静态求值成功</strong>
      <span>gap、padding 和颜色都来自模块顶层纯常量。</span>
    </div>
  );
}
