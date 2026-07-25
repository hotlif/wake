import { ReactElement, useState } from "react";
import _ from "lodash";
import type { CardProps, Stat } from "./types.js";

// 真实 JSX（wake 已支持 JSX automatic runtime 降级：jsx/jsxs/Fragment）。
// 这是带完整 TypeScript 类型的函数式组件——验证 wake 的完整 TSX 擦除。

// enum → 值转换（IIFE，正/反向映射）。
export enum Badge {
  New,
  Hot,
  Sale,
}

// 类型别名（联合 + 条件 + 映射，均擦除）。
type Labeled<T> = T extends { label: infer L } ? L : never;
type BadgeName = keyof typeof Badge;

const BADGE_TEXT: Record<number, string> = {
  [Badge.New]: "新",
  [Badge.Hot]: "热",
  [Badge.Sale]: "促",
};

export function Card(props: CardProps): ReactElement {
  const title: string = _.capitalize(props.title);
  const tags = props.tags ?? [];
  // 断言 + 泛型调用 + 可选链，均需完整擦除。
  const badge = (props.badge ?? Badge.New) as Badge;

  return (
    <section className="card">
      <h2>
        {title} <span className="badge">{BADGE_TEXT[badge]}</span>
      </h2>
      <p>{props.body}</p>
      <div className="tags">
        {tags.map((tag: string, i: number) => (
          <span className="tag" key={i}>
            {tag}
          </span>
        ))}
      </div>
    </section>
  );
}

export function StatsList(props: { stats: Stat[] }): ReactElement {
  return (
    <ul className="stats">
      {props.stats.map((s: Stat, i: number) => (
        <li key={i}>
          {s.label} = {s.value}
        </li>
      ))}
    </ul>
  );
}

// useState 验证 hooks；泛型 useState<number>() 验证调用类型实参擦除。
export function Counter(): ReactElement {
  const [count, setCount] = useState<number>(0);

  return (
    <div className="counter">
      <button type="button" onClick={() => setCount(count + 1)}>
        +1
      </button>
      <span className="count">点击次数：{count}</span>
    </div>
  );
}
