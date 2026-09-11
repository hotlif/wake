export function add(a: number, b: number): number {
  if (!Number.isFinite(a) || !Number.isFinite(b)) throw new Error("请输入有限数字");
  return a + b;
}
