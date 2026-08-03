import React from "react";
import DocumentedButton from "../crab-button";

export const meta = {
  title: "视觉层级",
  description: "用外观区分主要操作、次要操作和危险操作。",
  height: "auto",
  viewport: "responsive",
  background: "muted",
  padding: "lg",
  isolation: "iframe",
};

export default function ButtonAppearanceDemo() {
  return (
    <div style={{ display: "flex", flexWrap: "wrap", gap: 10 }}>
      <DocumentedButton appearance="primary">主要操作</DocumentedButton>
      <DocumentedButton appearance="subtle">次要操作</DocumentedButton>
      <DocumentedButton appearance="dashed">创建项目</DocumentedButton>
      <DocumentedButton appearance="danger">删除任务</DocumentedButton>
    </div>
  );
}
