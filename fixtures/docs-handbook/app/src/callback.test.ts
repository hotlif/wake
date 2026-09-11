import { expect, mock, test } from "@crab-dev/wake/test";

test("把结果通知调用者", () => {
  const notify = mock.fn<(value: number) => void>();
  notify(3);
  expect(notify).toHaveBeenCalledWith(3);
  expect(notify).toHaveBeenCalledTimes(1);
});
