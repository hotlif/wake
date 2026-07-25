// 仅通过 `import type` 消费的纯类型模块。
// wake 会把 `import type` 整条擦除且不产生依赖，因此本文件不会进入模块图。

export interface CardProps {
  title: string;
  body: string;
  tags?: string[];
  badge?: number;
}

export interface Stat {
  label: string;
  value: number;
}

export type Theme = "light" | "dark";
