import React from "react";
export default function Badge({ children }: { children: React.ReactNode }) {
  return <span style={{ display: "inline-flex", padding: "2px 8px", borderRadius: 999, color: "var(--wake-accent)", background: "color-mix(in srgb, var(--wake-accent) 10%, transparent)" }}>{children}</span>;
}
