import { build, WakeError } from "@crab-dev/wake";

try {
  const result = await build({ cwd: process.cwd(), outdir: "dist", cache: true });
  console.log("输出文件", result.files);
  for (const diagnostic of result.diagnostics) console.error(diagnostic.message);
} catch (error) {
  if (error instanceof WakeError) {
    console.error(error.code, error.message);
    for (const diagnostic of error.diagnostics ?? []) console.error(diagnostic);
  } else {
    console.error(error);
  }
  process.exitCode = 1;
}
