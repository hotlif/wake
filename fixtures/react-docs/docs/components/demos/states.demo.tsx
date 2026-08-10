import React from "react";
import Button from "@crab-dev/rc-button";

export const meta = {
  title: "常见状态",
  description: "展示次要操作、危险操作和禁用状态。",
  group: "基础组件",
  component: "按钮",
  order: 20,
  viewport: "responsive",
  background: "muted",
  padding: "lg",
};

export default function StatesDemo() {
  return <div style={{ display: "flex", gap: 12, flexWrap: "wrap" }}><Button appearance="subtle">稍后处理</Button><Button appearance="danger">删除</Button><Button disabled>暂不可用</Button></div>;
}
