import { useState } from "react";
import { createRoot } from "react-dom/client";
import "./style.css";

function App() {
  const [count, setCount] = useState(0);
  return <main>
    <h1>我的第一个 Wake 应用</h1>
    <button onClick={() => setCount(value => value + 1)}>点击次数：{count}</button>
  </main>;
}

const container = document.getElementById("root");
if (!container) throw new Error("缺少 #root 容器");
createRoot(container).render(<App />);
