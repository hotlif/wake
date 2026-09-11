import { expect, mock, test } from "@crab-dev/wake/test";

test("使用替换后的接口", async () => {
  mock.module("./profile-api", () => ({
    loadName: async () => "Ada",
  }));
  const { profileTitle } = await mock.import<typeof import("./profile")>("./profile");
  await expect(profileTitle()).resolves.toBe("用户：Ada");
});
