import React from 'react';
import ReactDOM from 'react-dom/client';
import App from './App';
import { ensureWindowStateWatcher } from './components/WindowControls';
import { isTauri } from './lib/tauri';
import './styles/theme.css';

if (isTauri()) {
  const isMac = typeof navigator !== 'undefined' && /Mac/.test(navigator.platform);
  if (!isMac) {
    document.body.classList.add('tauri-transparent');
    document.documentElement.classList.add('tauri-frameless');
  }
  ensureWindowStateWatcher();
}

ReactDOM.createRoot(document.getElementById('root') as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
