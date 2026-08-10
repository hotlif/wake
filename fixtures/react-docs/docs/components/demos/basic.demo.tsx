import React from "react";
import Button from "@crab-dev/rc-button";

/** 基础示例可在工作台中调整的属性。 */
interface BasicDemoProps {
  /** 按钮内显示的文字。 */
  children: string;
  /** 按钮的视觉样式。 */
  appearance?: "primary" | "subtle" | "dashed" | "text" | "link" | "danger";
  /** 按钮尺寸。 */
  size?: "large" | "middle" | "small";
  /** 是否显示加载状态。 */
  loading?: boolean;
  /** 是否禁用按钮。 */
  disabled?: boolean;
}

export const meta = {
  title: "基础用法",
  description: "用清晰的动作名称说明点击后会发生什么。",
  group: "基础组件",
  component: "按钮",
  order: 10,
  args: {
    children: "保存更改",
    size: "middle",
    loading: false,
    disabled: false,
  },
  height: "auto",
  viewport: "responsive",
  background: "surface",
  padding: "md",
  isolation: "iframe",
};

export default function BasicDemo(props: BasicDemoProps) {
  return <Button {...props} onClick={() => undefined} />;
}
