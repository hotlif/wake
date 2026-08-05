import React from "react";
import { createRoot } from "react-dom/client";
import { App } from "@wake/docs/runtime/app.tsx";
import { ComponentsApp } from "@wake/docs/runtime/components.tsx";
import { siteConfig } from "@wake/docs/config.tsx";
import "@wake/docs/runtime/styles.css";
import "@wake/docs/runtime/components.css";

const reactMajor = Number.parseInt(React.version.split(".")[0] || "", 10);
if (!Number.isFinite(reactMajor) || reactMajor < 19) {
  throw new Error(`Wake docs requires React 19 or newer; found React ${React.version || "unknown"}`);
}

const root = document.getElementById("root");
if (!root) throw new Error("Wake docs requires a #root element");
const isDemoFrame = new URLSearchParams(window.location.search).has("__wake_demo");
const Surface = siteConfig.mode === "components" && !isDemoFrame
  ? ComponentsApp : App;
createRoot(root).render(<React.StrictMode><Surface /></React.StrictMode>);
