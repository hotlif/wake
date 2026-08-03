import React from "react";
import { Button } from "../../../src/button.tsx";

export const meta = {
  title: "States",
  description: "Neutral, destructive, and disabled states.",
  viewport: "responsive",
  background: "muted",
  padding: "lg",
};

export default function StatesDemo() {
  return <div style={{ display: "flex", gap: 12, flexWrap: "wrap" }}><Button tone="neutral">Later</Button><Button tone="danger">Delete</Button><Button disabled>Unavailable</Button></div>;
}
