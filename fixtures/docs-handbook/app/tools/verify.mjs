import { build, runTests } from "@crab-dev/wake";

try {
  const tests = await runTests({ root: process.cwd(), patterns: ["src"] });
  if (!tests.success) {
    console.error("测试未通过", tests.counts);
    process.exitCode = 1;
  } else {
    const result = await build({ cwd: process.cwd(), outdir: "dist" });
    console.log(result.files);
  }
} catch (error) {
  console.error(error);
  process.exitCode = 1;
}
