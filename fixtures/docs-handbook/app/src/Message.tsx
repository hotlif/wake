import type { ReactNode } from "react";

export interface MessageProps {
  title: string;
  children?: ReactNode;
}

export function Message({ title, children }: MessageProps) {
  return <section><h2>{title}</h2>{children}</section>;
}
