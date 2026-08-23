import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import QmindStudio from './QmindStudio';

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <QmindStudio />
  </StrictMode>,
);