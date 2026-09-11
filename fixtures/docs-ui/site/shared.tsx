import React, { createContext, useContext } from 'react';
import { createPortal } from 'react-dom';
export const ExampleContext = createContext('default-context');
export function ContextProbe() { return <p data-testid="context">{useContext(ExampleContext)}</p>; }
export function InlineProbe() {
  return <div data-wake-demo><button data-testid="inline-probe" className="probe">Inline component</button>
    {createPortal(<button data-testid="portal-probe" className="probe">Portal component</button>, document.body)}
  </div>;
}
