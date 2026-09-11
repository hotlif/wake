import { expect, test } from "@crab-dev/wake/test";
import { render, screen, userEvent } from "@crab-dev/wake/test/react";
import { Counter } from "./Counter";

test("点击后显示新计数", async () => {
  const user = userEvent.setup();
  await render(<Counter />);
  await user.click(screen.getByRole("button", { name: "点击次数：0" }));
  expect(screen.getByRole("button", { name: "点击次数：1" })).toBeInTheDocument();
});
