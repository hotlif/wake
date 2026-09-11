import { expect, network, test } from "@crab-dev/wake/test";

test("读取接口结果", async () => {
  const dispose = network.route("https://example.test/user", () => ({
    status: 200,
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ name: "Ada" }),
  }));
  try {
    const response = await fetch("https://example.test/user");
    expect(await response.json()).toEqual({ name: "Ada" });
  } finally {
    dispose();
  }
});
