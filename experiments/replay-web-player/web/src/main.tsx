import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { App } from './App';
import './styles.css';

const worker = new URLSearchParams(location.hash.slice(1));
if (worker.has('render-job') && worker.has('worker')) {
  void import('./video/renderWorker').then(({runRenderWorker}) => runRenderWorker(worker.get('render-job')!,worker.get('worker')!));
} else createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);

