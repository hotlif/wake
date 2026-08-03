import React from "react";
import { Button } from "../../../src/button.tsx";

export const meta = {
  title: "Basic action",
  description: "The primary button treatment.",
  height: "auto",
  viewport: "responsive",
  background: "surface",
  padding: "md",
  isolation: "iframe",
};

export default function BasicDemo() {
  return <Button onClick={() => undefined}>Save changes</Button>;
}
