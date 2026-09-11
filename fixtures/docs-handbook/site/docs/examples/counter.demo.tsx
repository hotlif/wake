import { useState } from "react";

export const meta = {
  title: "计数器",
  description: "验证本地状态与交互",
  viewport: "responsive",
};

export default function CounterDemo() {
  const [count, setCount] = useState(0);
  return <button onClick={() => setCount(count + 1)}>
    已点击 {count} 次
  </button>;
}
