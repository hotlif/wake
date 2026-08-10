import React from "react";
import CrabButton from "@crab-dev/rc-button";
import type { ReactNode } from "react";

/** 文档站公开展示的 Crab UI 按钮属性子集。 */
export interface ButtonProps {
  /** 按钮内显示的内容。 */
  children: ReactNode;
  /**
   * 按钮的视觉样式。
   * @default "subtle"
   * @since 1.0.0
   */
  appearance?: "primary" | "subtle" | "dashed" | "text" | "link" | "danger";
  /** 按钮尺寸，默认为中等尺寸。 */
  size?: "large" | "middle" | "small";
  /** 是否显示加载状态。 */
  loading?: boolean;
  /** 是否禁用按钮。 */
  disabled?: boolean;
}

/** 使用 npm 发布的 Crab UI Button，供 MDX API 文档引用。 */
export function Button(props: ButtonProps) {
  return <CrabButton {...props} />;
}
