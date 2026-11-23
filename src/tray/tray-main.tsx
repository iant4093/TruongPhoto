import React from 'react';
import ReactDOM from 'react-dom/client';
import { TrayApp } from './TrayApp';
import '../styles/theme.css';

document.body.classList.add('tauri-transparent');
document.addEventListener('contextmenu', (event) => event.preventDefault());

ReactDOM.createRoot(document.getElementById('tray-root') as HTMLElement).render(
  <React.StrictMode>
    <TrayApp />
  </React.StrictMode>,
);
