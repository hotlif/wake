import React from "react";

export const meta = {
  title: "Beta basic",
  group: "Fixture",
  component: "Beta",
  args: { count: 2 },
};

export default function BetaDemo({ count }: { count: number }) {
  return <output>Beta {count}</output>;
}
