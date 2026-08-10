import React from "react";

export default function Preview({ children }: { children: React.ReactNode }) {
  return <div style={{ display: "grid", placeItems: "center", minHeight: "calc(100vh - 2px)", padding: 24 }}>{children}</div>;
}
