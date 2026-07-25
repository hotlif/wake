import { StrictMode, useMemo, useState } from 'react';
import type { CSSProperties, ReactNode } from 'react';
import { createRoot } from 'react-dom/client';

// ============================================================
// 枚举（值语义 → IIFE 降级）
// ============================================================

// 数字枚举：正向 + 反向映射
enum TaskStatus {
  Active,
  Completed,
  Archived,
}

// const 枚举（字符串成员）
const enum SortDirection {
  Asc = 'asc',
  Desc = 'desc',
}

// ============================================================
// 类型别名 / 接口（纯擦除）
// ============================================================

type Priority = 'low' | 'medium' | 'high';

// 模板字面量类型
type PriorityClass = `pri-${Priority}`;

// 具名元组 + readonly
type Bounds = readonly [min: number, max: number];

// 基接口 + 继承 + 只读 / 可选 / 索引签名成员
interface Entity {
  readonly id: number;
}

interface TodoItem extends Entity {
  title: string;
  done: boolean;
  priority: Priority;
  status: TaskStatus;
  tags?: readonly string[];
}

// keyof / 索引访问类型
type TodoField = keyof TodoItem;
type TodoTitle = TodoItem['title'];

// 映射类型
type Weights = { readonly [P in Priority]: number };

// 交叉类型 + 泛型
type Timestamped<T> = T & { readonly createdAt: number };

// 条件类型 + infer
type ElementOf<T> = T extends readonly (infer U)[] ? U : never;
type Item = ElementOf<TodoItem[]>;

// 函数类型别名（泛型）
type Comparator<T> = (a: T, b: T) => number;

// 联合字面量
type Filter = 'all' | 'active' | 'completed';

// typeof 类型查询
const RUNTIME_DEFAULTS = { priority: 'medium' as Priority, status: TaskStatus.Active };
type Defaults = typeof RUNTIME_DEFAULTS;

// 调用签名接口
interface CountFormatter {
  (value: number): string;
  readonly unit?: string;
}

// ============================================================
// declare 环境声明（整体擦除，无运行时）
// ============================================================

declare const __BRAND__: unique symbol;
type Brand<T, B extends string> = T & { readonly [__BRAND__]: B };
type TodoId = Brand<number, 'TodoId'>;

// ============================================================
// 常量断言 + satisfies
// ============================================================

const PRIORITY_WEIGHT = {
  low: 1,
  medium: 2,
  high: 3,
} as const satisfies Weights;

const FILTERS = ['all', 'active', 'completed'] as const satisfies readonly Filter[];

// ============================================================
// 命名空间（值语义 → IIFE 降级）
// ============================================================

namespace TaskUtils {
  export const weights: Weights = PRIORITY_WEIGHT;

  export function weightOf(todo: TodoItem): number {
    return weights[todo.priority];
  }

  // 泛型函数类型注解 + 箭头函数
  export const byPriority: Comparator<TodoItem> = (a, b) => weightOf(b) - weightOf(a);

  // 数字枚举反向查表
  export function statusLabel(status: TaskStatus): string {
    return TaskStatus[status] ?? 'Unknown';
  }

  export function classOf(priority: Priority): PriorityClass {
    return `pri-${priority}`;
  }
}

// ============================================================
// 泛型函数：约束 + 默认类型参数 + const 枚举默认值
// ============================================================

function sortBy<T, K extends keyof T = keyof T>(
  items: readonly T[],
  key: K,
  direction: SortDirection = SortDirection.Asc,
): T[] {
  const sign = direction === SortDirection.Asc ? 1 : -1;
  return [...items].sort((a, b) => {
    const av = a[key];
    const bv = b[key];
    if (av === bv) return 0;
    return av > bv ? sign : -sign;
  });
}

// ============================================================
// 类型谓词 + 断言函数
// ============================================================

function isHigh(todo: TodoItem): todo is TodoItem & { priority: 'high' } {
  return todo.priority === 'high';
}

function assertDefined<T>(value: T | null | undefined, label: string): asserts value is T {
  if (value == null) {
    throw new Error(`${label} 不应为空`);
  }
}

// ============================================================
// 抽象类：访问修饰符 / 参数属性 / 只读 / static / getter / 重载 / implements
// ============================================================

interface Collection<T> {
  readonly size: number;
  find(id: number): T | undefined;
}

abstract class BaseCollection<T extends Entity> implements Collection<T> {
  // declare 字段（纯类型，擦除）
  declare readonly kind: string;

  // 受保护构造 + 参数属性（→ this.items = items）
  protected constructor(protected readonly items: readonly T[]) {}

  // 抽象方法（无体，擦除）
  abstract describe(): string;

  get size(): number {
    return this.items.length;
  }

  find(id: number): T | undefined {
    return this.items.find((it) => it.id === id);
  }
}

class TodoCollection extends BaseCollection<TodoItem> {
  // 带初始化的 static 字段（保留）
  static readonly label: string = 'todos';

  constructor(items: readonly TodoItem[], public readonly name: string = 'todos') {
    super(items);
  }

  // 方法重载签名（擦除）+ 实现签名
  count(): number;
  count(priority: Priority): number;
  count(priority?: Priority): number {
    return priority ? this.items.filter((t) => t.priority === priority).length : this.size;
  }

  override describe(): string {
    return `${this.name} (${this.size})`;
  }

  static of(items: readonly TodoItem[]): TodoCollection {
    return new TodoCollection(items);
  }
}

// ============================================================
// 泛型 React 组件 + render-prop
// ============================================================

interface ListProps<T> {
  items: readonly T[];
  getKey: (item: T) => string | number;
  children: (item: T) => ReactNode;
  empty?: ReactNode;
}

function List<T>({ items, getKey, children, empty }: ListProps<T>) {
  if (items.length === 0) {
    return <>{empty ?? null}</>;
  }
  return <>{items.map((item) => <div key={getKey(item)}>{children(item)}</div>)}</>;
}

// ============================================================
// 数据 + 应用
// ============================================================

const initialTodos: TodoItem[] = [
  { id: 1, title: 'Review the design brief', done: true, priority: 'high', status: TaskStatus.Completed },
  { id: 2, title: 'Set up a quick React demo', done: false, priority: 'medium', status: TaskStatus.Active },
  { id: 3, title: 'Write a short summary', done: false, priority: 'low', status: TaskStatus.Active },
];

const cardStyle: CSSProperties = {
  background: '#fff',
  border: '1px solid #e2e8f0',
  borderRadius: 16,
  padding: 20,
};

function App() {
  const [todos, setTodos] = useState<TodoItem[]>(initialTodos);
  const [title, setTitle] = useState('');
  const [priority, setPriority] = useState<Priority>('medium');
  const [filter, setFilter] = useState<Filter>('all');
  const [sortDir, setSortDir] = useState<SortDirection>(SortDirection.Asc);

  const collection = useMemo(() => TodoCollection.of(todos), [todos]);

  const filteredTodos = useMemo(() => {
    const base =
      filter === 'active'
        ? todos.filter((todo) => !todo.done)
        : filter === 'completed'
          ? todos.filter((todo) => todo.done)
          : todos;
    // 泛型函数 + const 枚举方向
    return sortBy(base, 'title', sortDir);
  }, [filter, todos, sortDir]);

  const summary = useMemo(() => {
    return {
      completed: collection.count() - todos.filter((t) => !t.done).length,
      pending: todos.filter((t) => !t.done).length,
      highPriority: collection.count('high'),
    };
  }, [collection, todos]);

  const addTodo = () => {
    const trimmedTitle = title.trim();
    if (!trimmedTitle) return;

    setTodos((current) => [
      {
        id: Date.now(),
        title: trimmedTitle,
        done: false,
        priority,
        status: TaskStatus.Active,
      },
      ...current,
    ]);
    setTitle('');
    setPriority('medium');
  };

  const toggleTodo = (id: number) => {
    setTodos((current) =>
      current.map((todo) =>
        todo.id === id
          ? { ...todo, done: !todo.done, status: todo.done ? TaskStatus.Active : TaskStatus.Completed }
          : todo,
      ),
    );
  };

  const removeTodo = (id: number) => {
    setTodos((current) => current.filter((todo) => todo.id !== id));
  };

  const toggleSort = () =>
    setSortDir((dir) => (dir === SortDirection.Asc ? SortDirection.Desc : SortDirection.Asc));

  return (
    <div style={{ fontFamily: 'sans-serif', maxWidth: 760, margin: '40px auto', padding: 24 }}>
      <div
        style={{
          background: '#0f172a',
          color: '#fff',
          borderRadius: 16,
          padding: 24,
          marginBottom: 24,
          boxShadow: '0 10px 30px rgba(15,23,42,0.15)',
        }}
      >
        <h1 style={{ marginTop: 0 }}>Task Dashboard</h1>
        <p style={{ marginBottom: 0, color: '#cbd5e1' }}>
          A small React demo with state, filtering, and live summaries — {collection.describe()}.
        </p>
      </div>

      <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(180px, 1fr))', gap: 16, marginBottom: 24 }}>
        <StatCard label="Completed" value={summary.completed} />
        <StatCard label="Pending" value={summary.pending} />
        <StatCard label="High Priority" value={summary.highPriority} />
      </div>

      <section
        style={{
          background: '#fff',
          border: '1px solid #e2e8f0',
          borderRadius: 16,
          padding: 20,
          marginBottom: 24,
        }}
      >
        <h2 style={{ marginTop: 0 }}>Add a task</h2>
        <div style={{ display: 'flex', gap: 12, flexWrap: 'wrap' }}>
          <input
            value={title}
            onChange={(event) => setTitle(event.target.value)}
            placeholder="Enter a task title"
            style={{ flex: 1, minWidth: 220, padding: '10px 12px' }}
          />
          <select
            value={priority}
            onChange={(event) => setPriority(event.target.value as Priority)}
            style={{ padding: '10px 12px' }}
          >
            <option value="low">Low</option>
            <option value="medium">Medium</option>
            <option value="high">High</option>
          </select>
          <button onClick={addTodo} style={{ padding: '10px 16px', cursor: 'pointer' }}>
            Add
          </button>
        </div>
      </section>

      <section style={cardStyle}>
        <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', gap: 12, flexWrap: 'wrap' }}>
          <h2 style={{ margin: 0 }}>Task List</h2>
          <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
            {FILTERS.map((item) => (
              <button
                key={item}
                onClick={() => setFilter(item)}
                style={{
                  border: 'none',
                  padding: '8px 12px',
                  borderRadius: 999,
                  background: filter === item ? '#2563eb' : '#e2e8f0',
                  color: filter === item ? '#fff' : '#0f172a',
                  cursor: 'pointer',
                }}
              >
                {item}
              </button>
            ))}
            <button
              onClick={toggleSort}
              style={{ border: '1px solid #cbd5e1', padding: '8px 12px', borderRadius: 999, cursor: 'pointer' }}
            >
              Sort: {sortDir === SortDirection.Asc ? 'A→Z' : 'Z→A'}
            </button>
          </div>
        </div>

        <div style={{ marginTop: 16, display: 'grid', gap: 12 }}>
          <List
            items={filteredTodos}
            getKey={(todo) => todo.id}
            empty={
              <div style={{ padding: 16, color: '#64748b', textAlign: 'center' }}>
                No tasks match the current filter.
              </div>
            }
          >
            {(todo) => (
              <div
                style={{
                  display: 'flex',
                  alignItems: 'center',
                  justifyContent: 'space-between',
                  gap: 12,
                  border: '1px solid #e2e8f0',
                  borderRadius: 12,
                  padding: '12px 14px',
                }}
              >
                <div style={{ display: 'flex', alignItems: 'center', gap: 12 }}>
                  <input type="checkbox" checked={todo.done} onChange={() => toggleTodo(todo.id)} />
                  <div>
                    <div style={{ textDecoration: todo.done ? 'line-through' : 'none', fontWeight: 600 }}>
                      {todo.title}
                      {isHigh(todo) && (
                        <span
                          style={{
                            marginLeft: 8,
                            fontSize: 11,
                            fontWeight: 700,
                            color: '#b91c1c',
                            background: '#fee2e2',
                            borderRadius: 6,
                            padding: '2px 6px',
                          }}
                        >
                          HIGH
                        </span>
                      )}
                    </div>
                    <small style={{ color: '#64748b' }}>
                      Priority: {todo.priority} · Status: {TaskUtils.statusLabel(todo.status)} · Weight:{' '}
                      {TaskUtils.weightOf(todo)}
                    </small>
                  </div>
                </div>

                <button
                  onClick={() => removeTodo(todo.id)}
                  style={{ padding: '8px 12px', cursor: 'pointer' }}
                >
                  Remove
                </button>
              </div>
            )}
          </List>
        </div>
      </section>
    </div>
  );
}

function StatCard({ label, value }: { label: string; value: number }) {
  const format: CountFormatter = (n) => `${n}`;
  return (
    <div
      style={{
        background: '#fff',
        border: '1px solid #e2e8f0',
        borderRadius: 16,
        padding: 16,
        boxShadow: '0 6px 20px rgba(15,23,42,0.08)',
      }}
    >
      <div style={{ color: '#64748b', fontSize: 14 }}>{label}</div>
      <div style={{ fontSize: 28, fontWeight: 700, marginTop: 6 }}>{format(value)}</div>
    </div>
  );
}

const rootElement = document.getElementById('root');

// 断言函数：非空后 rootElement 收窄为 HTMLElement
assertDefined(rootElement, '#root');

createRoot(rootElement).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
