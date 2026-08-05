import React from "react";
import { Button, type ButtonProps } from "../../../src/button.tsx";

export const meta = {
  title: "Basic action",
  description: "The primary button treatment.",
  group: "Actions",
  component: "Button",
  order: 10,
  args: {
    children: "Save changes",
    tone: "primary",
    compact: false,
    destructive: false,
  },
  height: "auto",
  viewport: "responsive",
  background: "surface",
  padding: "md",
  isolation: "iframe",
};

export default function BasicDemo(props: ButtonProps) {
  return <Button {...props} onClick={() => undefined} />;
}
