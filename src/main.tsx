import React from 'react';
import ReactDOM from 'react-dom/client';
import App from './App';
import { isTauri } from './lib/tauri';
import './styles/theme.css';

if (isTauri()) {
  const isMac = typeof navigator !== 'undefined' && /Mac/.test(navigator.platform);
  if (!isMac) {
    document.body.classList.add('tauri-transparent');
  }
  document.addEventListener('contextmenu', (event) => {
    const target = event.target as HTMLElement | null;
    if (
      target &&
      (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA' || target.isContentEditable)
    ) {
      return;
    }
    event.preventDefault();
  });
}

ReactDOM.createRoot(document.getElementById('root') as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
