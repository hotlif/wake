import React from "react";
import DocumentedButton from "../crab-button";

export const meta = {
  title: "状态与宽度",
  description: "加载、禁用和整行按钮保持清晰的交互反馈。",
  height: "auto",
  viewport: "responsive",
  background: "surface",
  padding: "lg",
  isolation: "iframe",
};

export default function ButtonStatesDemo() {
  return (
    <div style={{ display: "grid", width: "min(100%, 420px)", gap: 10 }}>
      <DocumentedButton loading>正在构建</DocumentedButton>
      <DocumentedButton appearance="subtle" disabled>当前不可用</DocumentedButton>
      <DocumentedButton fullWidth showArrow>查看构建报告</DocumentedButton>
    </div>
  );
}
