import { expect, test } from "@crab-dev/wake/test";
import { add } from "./add";

test("两个数字相加", () => {
  expect(add(2, 3)).toBe(5);
});

test("拒绝无限值", () => {
  expect(() => add(Infinity, 1)).toThrow("请输入有限数字");
});
