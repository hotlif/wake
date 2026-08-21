import React from "react";

export const meta = {
  title: "PnP workspace demo",
  group: "PnP",
  component: "Workspace",
  args: { label: "PnP" },
};

export default function WorkspaceDemo({ label }: { label: string }) {
  return <button type="button">{label}</button>;
}
