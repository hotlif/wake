import { clock, expect, test } from "@crab-dev/wake/test";

test("到达延时后执行", async () => {
  await clock.fake();
  try {
    let ready = false;
    setTimeout(() => { ready = true; }, 1000);
    await clock.advanceBy(999);
    expect(ready).toBe(false);
    await clock.advanceBy(1);
    expect(ready).toBe(true);
  } finally {
    await clock.restore();
  }
});
