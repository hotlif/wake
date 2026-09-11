import { expect, test } from "@crab-dev/wake/test";

test("结果具有需要的结构", () => {
  const user = { id: 1, name: "Ada", roles: ["reader"] };
  expect(user.id).toBe(1);
  expect(user).toEqual({ id: 1, name: "Ada", roles: ["reader"] });
  expect(user.roles).toContain("reader");
});

test("等待 Promise 成功和失败", async () => {
  await expect(Promise.resolve({ saved: true })).resolves.toEqual({ saved: true });
  await expect(Promise.reject(new Error("保存失败"))).rejects.toThrow("保存失败");
});
