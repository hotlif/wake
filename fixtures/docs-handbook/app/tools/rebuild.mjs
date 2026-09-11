import { createBuildContext } from "@crab-dev/wake";

const context = await createBuildContext({ cwd: process.cwd(), outdir: "dist" });
try {
  const first = await context.rebuild();
  console.log(first.files);
  const second = await context.rebuild();
  console.log(second.files);
} finally {
  await context.close();
}
