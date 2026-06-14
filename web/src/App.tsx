import { useState, useEffect, useCallback, useRef } from 'react';
import type { AppConfig, Status } from './lib/api';
import * as api from './lib/api';
import { checkForUpdate, installUpdate } from './lib/updater';
import type { UpdateInfo } from './lib/updater';
import { LoginPage } from './pages/LoginPage';
import { ProvidersPage } from './pages/ProvidersPage';
import { RoutesPage } from './pages/RoutesPage';
import { TagsPage } from './pages/TagsPage';
import { LogsPage } from './pages/LogsPage';
import { ApiKeysPage } from './pages/ApiKeysPage';
import { UsagePage } from './pages/UsagePage';
import { StatusDot } from './components/StatusDot';

type Tab = 'logs' | 'providers' | 'routes' | 'tags' | 'keys' | 'usage';

const tabs: { key: Tab; label: string; icon: string }[] = [
  { key: 'logs', label: 'Logs', icon: '📋' },
  { key: 'usage', label: 'Usage', icon: '📊' },
  { key: 'keys', label: 'API Keys', icon: '🔑' },
  { key: 'routes', label: 'Routes', icon: '🔀' },
  { key: 'providers', label: 'Providers', icon: '🔌' },
  { key: 'tags', label: 'Tags', icon: '🏷️' },
];

function App() {
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [status, setStatus] = useState<Status | null>(null);
  const [tab, setTab] = useState<Tab>('logs');
  const [error, setError] = useState<string | null>(null);
  const [updateInfo, setUpdateInfo] = useState<UpdateInfo | null>(null);
  const [updating, setUpdating] = useState(false);
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  const [admin, setAdmin] = useState<string | null | undefined>(undefined);

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
    checkAuth();
    checkForUpdate().then(info => {
      if (info) setUpdateInfo(info);
    });
  }, [checkAuth]);

  const handleLogin = async (username: string, password: string) => {
    try {
      await api.adminLogin(username, password);
      await checkAuth();
    } catch (e) {
      showError(e instanceof Error ? e.message : 'Login failed');
    }
  };

  const handleLogout = async () => {
    await api.adminLogout();
    setAdmin(null);
    setConfig(null);
    setStatus(null);
  };

  const fileInputRef = useRef<HTMLInputElement>(null);

  const handleExport = useCallback(async () => {
    try {
      const exportData = await api.exportConfig();
      const json = JSON.stringify(exportData, null, 2);
      const blob = new Blob([json], { type: 'application/json' });
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = 'aginxbrain-config.json';
      document.body.appendChild(a);
      a.click();
      document.body.removeChild(a);
      URL.revokeObjectURL(url);
    } catch (e) {
      showError(e instanceof Error ? e.message : 'Export failed');
    }
  }, [showError]);

  const handleImport = useCallback(() => {
    fileInputRef.current?.click();
  }, []);

  const handleFileSelected = useCallback(async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;
    try {
      const text = await file.text();
      const importedConfig = JSON.parse(text) as AppConfig;
      await api.importConfig(importedConfig);
      await refresh();
    } catch (e) {
      showError(e instanceof Error ? e.message : 'Import failed');
    } finally {
      if (fileInputRef.current) {
        fileInputRef.current.value = '';
      }
    }
  }, [refresh, showError]);

  const handleCheckUpdate = useCallback(async () => {
    setCheckingUpdate(true);
    try {
      const info = await checkForUpdate();
      if (info) {
        setUpdateInfo(info);
      } else {
        showError('Already up to date');
      }
    } catch {
      showError('Update check failed');
    }
    setCheckingUpdate(false);
  }, [showError]);

  if (admin === undefined || (admin && (!config || !status))) {
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
    return <LoginPage onLogin={handleLogin} setupRequired={status?.setup_required ?? true} />;
  }

  const handleConfigChange = (newConfig: AppConfig) => {
    setConfig(newConfig);
  };

  const handleTakeoverToggle = async () => {
    try {
      if (status!.takeover.active) {
        await api.restoreClaude();
      } else {
        await api.takeoverClaude();
      }
      await refresh();
    } catch (e) {
      showError(e instanceof Error ? e.message : 'Takeover failed');
    }
  };

  const handleCodexTakeoverToggle = async () => {
    try {
      if (status!.codex_takeover.active) {
        await api.restoreCodex();
      } else {
        await api.takeoverCodex();
      }
      await refresh();
    } catch (e) {
      showError(e instanceof Error ? e.message : 'Codex takeover failed');
    }
  };

  return (
    <div style={{ maxWidth: 1100, margin: '0 auto', padding: '20px 24px', minHeight: '100vh' }}>
      {/* Hidden file input for import */}
      <input
        ref={fileInputRef}
        type="file"
        accept=".json"
        style={{ display: 'none' }}
        onChange={handleFileSelected}
      />
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
          <button
            onClick={handleCheckUpdate}
            disabled={checkingUpdate}
            style={{
              background: 'transparent', color: 'var(--text-secondary)',
              border: '1px solid var(--border)', borderRadius: 'var(--radius-sm)',
              padding: '4px 10px', fontSize: 12, cursor: checkingUpdate ? 'default' : 'pointer', fontWeight: 500,
              opacity: checkingUpdate ? 0.5 : 1,
            }}
          >{checkingUpdate ? '⏳ Checking...' : '↑ Update'}</button>
          <div style={{ display: 'flex', gap: 6 }}>
            <button
              onClick={handleImport}
              style={{
                background: 'transparent', color: 'var(--text-secondary)',
                border: '1px solid var(--border)', borderRadius: 'var(--radius-sm)',
                padding: '4px 10px', fontSize: 12, cursor: 'pointer', fontWeight: 500,
              }}
            >Import</button>
            <button
              onClick={handleExport}
              style={{
                background: 'transparent', color: 'var(--text-secondary)',
                border: '1px solid var(--border)', borderRadius: 'var(--radius-sm)',
                padding: '4px 10px', fontSize: 12, cursor: 'pointer', fontWeight: 500,
              }}
            >Export</button>
          </div>
          <div style={{ display: 'flex', alignItems: 'center', gap: 4 }}>
            <StatusDot active={status!.takeover.active} />
            <button
              onClick={handleTakeoverToggle}
              style={{
                background: status!.takeover.active ? 'var(--accent)' : 'transparent',
                color: status!.takeover.active ? '#fff' : 'var(--text-secondary)',
                border: '1px solid var(--border)', borderRadius: 'var(--radius-sm)',
                padding: '4px 10px', fontSize: 12, cursor: 'pointer', fontWeight: 500,
              }}
            >Claude Code</button>
          </div>
          <div style={{ display: 'flex', alignItems: 'center', gap: 4 }}>
            <StatusDot active={status!.codex_takeover.active} />
            <button
              onClick={handleCodexTakeoverToggle}
              style={{
                background: status!.codex_takeover.active ? 'var(--accent)' : 'transparent',
                color: status!.codex_takeover.active ? '#fff' : 'var(--text-secondary)',
                border: '1px solid var(--border)', borderRadius: 'var(--radius-sm)',
                padding: '4px 10px', fontSize: 12, cursor: 'pointer', fontWeight: 500,
              }}
            >Codex</button>
          </div>
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

      {/* Update banner */}
      {updateInfo && (
        <div style={{
          background: 'rgba(34, 197, 94, 0.1)', color: '#22C55E', border: '1px solid rgba(34, 197, 94, 0.2)',
          borderRadius: 'var(--radius-sm)', padding: '10px 14px', marginBottom: 16, fontSize: 13,
          display: 'flex', justifyContent: 'space-between', alignItems: 'center',
        }}>
          <span>New version available: {updateInfo.version}</span>
          <button
            onClick={async () => {
              setUpdating(true);
              try {
                await installUpdate(updateInfo);
              } catch (e) {
                showError(e instanceof Error ? e.message : 'Update failed');
              }
              setUpdating(false);
            }}
            disabled={updating}
            style={{
              background: '#22C55E', color: '#fff', border: 'none', borderRadius: 'var(--radius-sm)',
              padding: '4px 10px', fontSize: 12, cursor: updating ? 'default' : 'pointer', fontWeight: 500,
            }}
          >{updating ? 'Installing...' : 'Install'}</button>
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
      {tab === 'logs' && <LogsPage />}
      {tab === 'usage' && <UsagePage />}
      {tab === 'keys' && <ApiKeysPage />}
      {tab === 'providers' && <ProvidersPage config={config!} onConfigChange={handleConfigChange} />}
      {tab === 'routes' && <RoutesPage config={config!} onConfigChange={handleConfigChange} />}
      {tab === 'tags' && <TagsPage config={config!} onConfigChange={handleConfigChange} />}
    </div>
  );
}

export default App;
