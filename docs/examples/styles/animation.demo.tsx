import React, { useState } from "react";
import { css, keyframes } from "@crab-dev/css";

export const meta = {
  title: "局部动画",
  description: "重新播放局部 keyframes；减少动态效果时自动停用。",
  order: 40,
  viewport: "responsive",
  background: "muted",
  padding: "lg",
};

const enter = keyframes`
  from { opacity: 0; transform: translateY(8px) scale(.98); }
  to { opacity: 1; transform: translateY(0) scale(1); }
`;

const layout = css`
  display: grid;
  justify-items: start;
  gap: 14px;
`;

const panel = css`
  padding: 18px;
  color: #312e81;
  border: 1px solid #c4b5fd;
  border-radius: 12px;
  background: #f5f3ff;
  animation: ${enter} 240ms ease-out;

  @media (prefers-reduced-motion: reduce) {
    animation: none;
  }
`;

const action = css`
  padding: 7px 12px;
  border: 1px solid #a78bfa;
  border-radius: 8px;
  background: #fff;
  cursor: pointer;
`;

export default function AnimationDemo() {
  const [iteration, setIteration] = useState(0);
  return (
    <div className={layout}>
      <div className={panel} key={iteration}>内容始终可读，动画只是辅助反馈。</div>
      <button className={action} type="button" onClick={() => setIteration((value) => value + 1)}>重新播放</button>
    </div>
  );
}
