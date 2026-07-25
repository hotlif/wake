import _ from "lodash";
import type { Stat } from "./types";

// 泛型 + 返回类型注解（函数声明形式，均可被 wake 擦除）。
export function summarize<T>(items: T[]): string {
  return "共 " + items.length + " 项";
}

// 使用 lodash（纯 CJS 库，无需插件）：kebabCase 生成 slug。
export function slugify(text: string): string {
  return _.kebabCase(text);
}

// 取前 n 大：orderBy + take。
export function pickTop(nums: number[], n: number): number[] {
  const sorted = _.orderBy(nums, [], ["desc"]);
  return _.take(sorted, n);
}

// 聚合统计，返回带类型注解的对象数组。
export function buildStats(nums: number[]): Stat[] {
  const stats: Stat[] = [
    { label: "sum", value: _.sum(nums) },
    { label: "max", value: _.max(nums) as number },
    { label: "uniq", value: _.uniq(nums).length },
  ];
  return stats;
}
