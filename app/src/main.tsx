import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { SettingsView } from './Settings';
import './styles.css';

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <SettingsView />
  </StrictMode>,
);
