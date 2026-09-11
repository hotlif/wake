import React, { useState } from 'react';
import Button from '@crab-dev/rc-button';
export const meta = { title: 'Counter' };
export default function Counter() {
  const [count, setCount] = useState(0);
  return <Button onClick={() => setCount(count + 1)}>Count {count}</Button>;
}
