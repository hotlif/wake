import React from "react";

export const meta = {
  title: "Alpha basic",
  group: "Fixture",
  component: "Alpha",
  args: { label: "Alpha" },
};

export default function AlphaDemo({ label }: { label: string }) {
  return <button type="button">{label}</button>;
}
