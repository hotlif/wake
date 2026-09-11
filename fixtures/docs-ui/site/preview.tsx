import React from 'react';
export default function Preview({ children }: { children: React.ReactNode }) {
  return <div data-testid="legacy-preview" style={{ padding: 16, background: 'white', color: 'black' }}>{children}</div>;
}
