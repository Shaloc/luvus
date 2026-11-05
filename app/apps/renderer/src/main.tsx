import { Component, type ErrorInfo, type ReactNode } from 'react';
import { createRoot } from 'react-dom/client';
import { App } from './App';
import './index.css';

class DesktopBoundary extends Component<{ children: ReactNode }, { error: string | null }> {
  state: { error: string | null } = { error: null };
  static getDerivedStateFromError(error: Error) { return { error: error.message }; }
  componentDidCatch(_error: Error, _info: ErrorInfo) { /* Do not log snapshot or terminal contents. */ }
  render() {
    if (this.state.error) return <main className="empty-state" role="alert"><h2>The desktop view could not be rendered</h2><p>{this.state.error}</p><p>Your core server and terminals have not been stopped.</p><button className="button" onClick={() => window.location.reload()}>Reload this client</button></main>;
    return this.props.children;
  }
}
const root = document.getElementById('root');
if (!root) throw new Error('Desktop root element is missing.');
createRoot(root).render(<DesktopBoundary><App /></DesktopBoundary>);
