import { useState, useEffect, useCallback } from 'react';
import type { AppConfig, Status } from './lib/api';
import * as api from './lib/api';
import { LoginPage } from './pages/LoginPage';
import { LandingPage } from './pages/LandingPage';
import { LogsPage } from './pages/LogsPage';
import { ApiKeysPage } from './pages/ApiKeysPage';
import { ProvidersPage } from './pages/ProvidersPage';
import { RoutesPage } from './pages/RoutesPage';
import { TagsPage } from './pages/TagsPage';

type Tab = 'dashboard' | 'keys' | 'providers' | 'routes' | 'tags';

const tabs: { key: Tab; label: string; icon: string }[] = [
  { key: 'dashboard', label: 'Dashboard', icon: '📊' },
  { key: 'keys', label: 'API Keys', icon: '🔑' },
  { key: 'providers', label: 'Providers', icon: '🔌' },
  { key: 'routes', label: 'Routes', icon: '🔀' },
  { key: 'tags', label: 'Tags', icon: '🏷️' },
];

function App() {
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [status, setStatus] = useState<Status | null>(null);
  const [tab, setTab] = useState<Tab>('dashboard');
  const [error, setError] = useState<string | null>(null);
  const [admin, setAdmin] = useState<string | null | undefined>(undefined);
  const [showLogin, setShowLogin] = useState(false);

  const showError = useCallback((msg: string) => {
    setError(msg);
    setTimeout(() => setError(null), 4000);
  }, []);

  const refresh = useCallback(async () => {
    const [c, s] = await Promise.all([api.getConfig(), api.getStatus()]);
    setConfig(c);
    setStatus(s);
  }, []);

  const checkAuth = useCallback(async () => {
    try {
      const me = await api.getMe();
      setAdmin(me.username);
      await refresh();
    } catch {
      setAdmin(null);
    }
  }, [refresh]);

  useEffect(() => {
    api.getStatus().then(setStatus).catch(() => {});
    checkAuth();
  }, [checkAuth]);

  const handleLogin = async (username: string, password: string, isSetup: boolean) => {
    try {
      if (isSetup) {
        await api.adminSetup(username, password);
      } else {
        await api.adminLogin(username, password);
      }
      await checkAuth();
    } catch (e) {
      showError(e instanceof Error ? e.message : (isSetup ? 'Setup failed' : 'Login failed'));
    }
  };

  const handleLogout = async () => {
    await api.adminLogout();
    setAdmin(null);
    setConfig(null);
    setStatus(null);
    setShowLogin(false);
  };

  const handleConfigChange = (newConfig: AppConfig) => {
    setConfig(newConfig);
  };

  if (admin === undefined || !status || (admin && !config)) {
    return (
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', minHeight: '100vh', color: 'var(--text-muted)' }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
          <span style={{ width: 20, height: 20, border: '2px solid var(--border)', borderTopColor: 'var(--accent)', borderRadius: '50%', animation: 'spin 0.8s linear infinite', display: 'inline-block' }} />
          Loading...
        </div>
        <style>{`@keyframes spin { to { transform: rotate(360deg) } }`}</style>
      </div>
    );
  }

  if (!admin) {
    if (!showLogin) {
      return <LandingPage onEnter={() => setShowLogin(true)} setupRequired={status?.setup_required ?? true} />;
    }
    return <LoginPage onLogin={handleLogin} setupRequired={status?.setup_required ?? true} />;
  }

  return (
    <div style={{ maxWidth: 1100, margin: '0 auto', padding: '20px 24px', minHeight: '100vh' }}>
      {/* Header */}
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 20 }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
          <div style={{
            width: 32, height: 32, borderRadius: 'var(--radius-sm)',
            background: 'linear-gradient(135deg, var(--accent), #8B5CF6)',
            display: 'flex', alignItems: 'center', justifyContent: 'center',
            fontSize: 16, fontWeight: 700, color: '#fff',
          }}>M</div>
          <div>
            <h1 style={{ margin: 0, fontSize: 18, fontWeight: 700, letterSpacing: '-0.3px' }}>AginxBrain</h1>
            <div style={{ fontSize: 11, color: 'var(--text-muted)', fontFamily: 'monospace' }}>{config!.host}:{config!.port}</div>
          </div>
        </div>
        <div style={{ display: 'flex', alignItems: 'center', gap: 12 }}>
          <span style={{ fontSize: 12, color: 'var(--text-muted)' }}>{admin}</span>
          <button onClick={handleLogout} style={{
            background: 'transparent', color: 'var(--text-secondary)',
            border: '1px solid var(--border)', borderRadius: 'var(--radius-sm)',
            padding: '4px 10px', fontSize: 12, cursor: 'pointer', fontWeight: 500,
          }}>Logout</button>
        </div>
      </div>

      {/* Error banner */}
      {error && (
        <div style={{
          background: 'rgba(239, 68, 68, 0.1)', color: '#EF4444', border: '1px solid rgba(239, 68, 68, 0.2)',
          borderRadius: 'var(--radius-sm)', padding: '10px 14px', marginBottom: 16, fontSize: 13,
        }}>
          {error}
        </div>
      )}

      {/* Tabs */}
      <div style={{ display: 'flex', gap: 8, borderBottom: '1px solid var(--border)', marginBottom: 20 }}>
        {tabs.map(t => (
          <button
            key={t.key}
            onClick={() => setTab(t.key)}
            style={{
              background: 'transparent', border: 'none', borderBottom: tab === t.key ? '2px solid var(--accent)' : '2px solid transparent',
              color: tab === t.key ? 'var(--text-primary)' : 'var(--text-secondary)',
              padding: '10px 14px', fontSize: 13, cursor: 'pointer', fontWeight: 500,
              display: 'flex', alignItems: 'center', gap: 6,
            }}
          >
            <span>{t.icon}</span>
            {t.label}
          </button>
        ))}
      </div>

      {/* Tab content */}
      {tab === 'dashboard' && <LogsPage />}
      {tab === 'keys' && <ApiKeysPage />}
      {tab === 'providers' && <ProvidersPage config={config!} onConfigChange={handleConfigChange} />}
      {tab === 'routes' && <RoutesPage config={config!} onConfigChange={handleConfigChange} />}
      {tab === 'tags' && <TagsPage config={config!} onConfigChange={handleConfigChange} />}
    </div>
  );
}

export default App;
