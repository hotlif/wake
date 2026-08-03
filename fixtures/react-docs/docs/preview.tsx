import React from "react";

export default function Preview({ children }: { children: React.ReactNode }) {
  return <div style={{ display: "grid", placeItems: "center", minHeight: 96 }}>{children}</div>;
}
