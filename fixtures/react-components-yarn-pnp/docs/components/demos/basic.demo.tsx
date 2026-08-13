import React from "react";

interface BasicDemoProps {
  children: string;
  disabled?: boolean;
}

export const meta = {
  title: "基础用法",
  description: "不直接依赖 Crab UI 的最小组件示例。",
  group: "基础组件",
  component: "按钮",
  order: 10,
  args: {
    children: "保存更改",
    disabled: false,
  },
  viewport: "responsive",
  background: "surface",
  padding: "md",
  isolation: "iframe",
};

export default function BasicDemo(props: BasicDemoProps) {
  return <button type="button" disabled={props.disabled}>{props.children}</button>;
}
