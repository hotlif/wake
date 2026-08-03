import React from "react";
import { createRoot } from "react-dom/client";
import { App } from "@wake/docs/runtime/app.tsx";
import "@wake/docs/runtime/styles.css";

const reactMajor = Number.parseInt(React.version.split(".")[0] || "", 10);
if (!Number.isFinite(reactMajor) || reactMajor < 19) {
  throw new Error(`Wake docs requires React 19 or newer; found React ${React.version || "unknown"}`);
}

const root = document.getElementById("root");
if (!root) throw new Error("Wake docs requires a #root element");
createRoot(root).render(<React.StrictMode><App /></React.StrictMode>);
