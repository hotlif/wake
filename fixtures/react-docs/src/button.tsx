import React from "react";
import type { ButtonHTMLAttributes, ReactNode } from "react";
import "./button.css";

export interface ButtonProps extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, "color"> {
  /** Content rendered inside the button. */
  children: ReactNode;
  /**
   * Visual emphasis.
   * @default "primary"
   * @since 1.0.0
   */
  tone?: "primary" | "neutral" | "danger";
  /** Displays a compact control. */
  compact?: boolean;
  /**
   * Legacy intent flag.
   * @deprecated Use tone="danger".
   */
  destructive?: boolean;
}

export function Button({ children, tone = "primary", compact = false, destructive = false, ...props }: ButtonProps) {
  const resolvedTone = destructive ? "danger" : tone;
  return <button className={"fixture-button fixture-button-" + resolvedTone + (compact ? " is-compact" : "")} {...props}>{children}</button>;
}
