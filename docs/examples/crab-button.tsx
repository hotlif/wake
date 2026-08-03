import React from "react";
import Button from "@crab-dev/rc-button";

/**
 * 文档站用于讲解 Crab UI Button 的稳定属性集合。
 *
 * 完整上游属性仍以 @crab-dev/rc-button 导出的 ButtonProps 为准。
 */
export interface DocumentedButtonProps {
  /**
   * 按钮中显示的文字或元素。
   * @default "开始构建"
   */
  children?: React.ReactNode;
  /**
   * 按钮的视觉层级。
   * @default "primary"
   */
  appearance?: "primary" | "subtle" | "dashed" | "danger";
  /**
   * 按钮尺寸。
   * @default "middle"
   */
  size?: "small" | "middle" | "large";
  /**
   * 是否显示加载状态并阻止重复操作。
   * @default false
   */
  loading?: boolean;
  /**
   * 是否禁用按钮。
   * @default false
   */
  disabled?: boolean;
  /**
   * 是否占满可用宽度。
   * @default false
   */
  fullWidth?: boolean;
  /**
   * 是否在文字后显示方向箭头。
   * @default false
   */
  showArrow?: boolean;
  /** 用户激活按钮时执行的回调。 */
  onClick?: () => void;
}

export default function DocumentedButton({
  children = "开始构建",
  appearance = "primary",
  size = "middle",
  loading = false,
  disabled = false,
  fullWidth = false,
  showArrow = false,
  onClick,
}: DocumentedButtonProps) {
  return (
    <Button
      appearance={appearance}
      size={size}
      loading={loading}
      disabled={disabled}
      shouldFitContainer={fullWidth}
      iconAfter={showArrow ? <span aria-hidden="true">→</span> : undefined}
      onClick={onClick}
    >
      {children}
    </Button>
  );
}
