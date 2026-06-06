import { useState, useEffect, useCallback, useRef } from 'react';
import type { AppConfig, Status } from './lib/api';
import * as api from './lib/api';
import { checkForUpdate, installUpdate } from './lib/updater';
import type { UpdateInfo } from './lib/updater';
import { ProvidersPage } from './pages/ProvidersPage';
import { RoutesPage } from './pages/RoutesPage';
import { TagsPage } from './pages/TagsPage';
import { LogsPage } from './pages/LogsPage';
import { StatusDot } from './components/StatusDot';

type Tab = 'logs' | 'providers' | 'routes' | 'tags';

const tabs: { key: Tab; label: string; icon: string }[] = [
  { key: 'logs', label: 'Logs', icon: '📋' },
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

  const showError = useCallback((msg: string) => {
    setError(msg);
    setTimeout(() => setError(null), 4000);
  }, []);

  const refresh = useCallback(async () => {
    const [c, s] = await Promise.all([api.getConfig(), api.getStatus()]);
    setConfig(c);
    setStatus(s);
  }, []);

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

  useEffect(() => {
    refresh();
    checkForUpdate().then(info => {
      if (info) setUpdateInfo(info);
    });
  }, [refresh]);

  if (!config || !status) {
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

  const handleConfigChange = (newConfig: AppConfig) => {
    setConfig(newConfig);
  };

  const handleTakeoverToggle = async () => {
    try {
      if (status.takeover.active) {
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
      if (status.codex_takeover.active) {
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
    <div style={{ maxWidth: 960, margin: '0 auto', padding: '20px 24px', minHeight: '100vh' }}>
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
            <div style={{ fontSize: 11, color: 'var(--text-muted)', fontFamily: 'monospace' }}>127.0.0.1:{config.port}</div>
          </div>
        </div>
        <div style={{ display: 'flex', alignItems: 'center', gap: 12 }}>
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
            <StatusDot active={status.takeover.active} />
            <span style={{ fontSize: 11, color: 'var(--text-muted)', fontFamily: 'monospace' }}>Claude</span>
          </div>
          <div style={{ display: 'flex', alignItems: 'center', gap: 4 }}>
            <StatusDot active={status.codex_takeover.active} />
            <span style={{ fontSize: 11, color: 'var(--text-muted)', fontFamily: 'monospace' }}>Codex</span>
          </div>
        </div>
      </div>

      {/* Update notification */}
      {updateInfo && !updating && (
        <div style={{
          padding: '8px 14px', marginBottom: 12, borderRadius: 'var(--radius-md)',
          background: 'rgba(34,197,94,0.1)', border: '1px solid rgba(34,197,94,0.2)',
          fontSize: 13, display: 'flex', alignItems: 'center', gap: 12,
        }}>
          <span style={{ fontSize: 14, flexShrink: 0 }}>↑</span>
          <span style={{ flex: 1 }}>
            New version <strong>v{updateInfo.version}</strong> available
          </span>
          <button
            onClick={async () => {
              setUpdating(true);
              try {
                await installUpdate(updateInfo);
              } catch (e) {
                setUpdating(false);
                setUpdateInfo(null);
                showError(e instanceof Error ? e.message : 'Update failed');
              }
            }}
            style={{
              background: 'var(--success)', color: '#fff', border: 'none',
              borderRadius: 'var(--radius-sm)', padding: '4px 14px',
              fontSize: 12, cursor: 'pointer', fontWeight: 600,
            }}
          >Update</button>
          <span
            onClick={() => setUpdateInfo(null)}
            style={{ cursor: 'pointer', opacity: 0.5, fontSize: 14, flexShrink: 0 }}
          >x</span>
        </div>
      )}

      {updating && (
        <div style={{
          padding: '8px 14px', marginBottom: 12, borderRadius: 'var(--radius-md)',
          background: 'rgba(59,130,246,0.1)', border: '1px solid rgba(59,130,246,0.2)',
          color: 'var(--accent)', fontSize: 13, display: 'flex', alignItems: 'center', gap: 8,
        }}>
          <span style={{ width: 14, height: 14, border: '2px solid var(--accent)', borderTopColor: 'transparent', borderRadius: '50%', animation: 'spin 0.8s linear infinite', display: 'inline-block' }} />
          Downloading update...
        </div>
      )}

      {/* Error display */}
      {error && (
        <div style={{
          padding: '8px 14px', marginBottom: 12, borderRadius: 'var(--radius-md)',
          background: 'rgba(239,68,68,0.1)', border: '1px solid rgba(239,68,68,0.2)',
          color: '#ef4444', fontSize: 13, display: 'flex', alignItems: 'center', gap: 8,
        }}>
          <span style={{ fontSize: 14, flexShrink: 0 }}>!</span>
          <span style={{ flex: 1 }}>{error}</span>
          <span
            onClick={() => setError(null)}
            style={{ cursor: 'pointer', opacity: 0.6, fontSize: 14, flexShrink: 0 }}
          >x</span>
        </div>
      )}

      {/* Takeover bar — both switches side by side */}
      <div style={{
        display: 'flex', gap: 12, marginBottom: 20,
      }}>
        {/* Claude Code Takeover */}
        <div style={{
          flex: 1, display: 'flex', justifyContent: 'space-between', alignItems: 'center',
          padding: '10px 16px',
          background: status.takeover.active ? 'var(--success-dim)' : 'var(--bg-card)',
          border: `1px solid ${status.takeover.active ? 'rgba(34,197,94,0.2)' : 'var(--border)'}`,
          borderRadius: 'var(--radius-md)',
        }}>
          <span style={{ fontSize: 13, color: status.takeover.active ? 'var(--success)' : 'var(--text-secondary)' }}>
            Claude Code
          </span>
          <div
            onClick={handleTakeoverToggle}
            style={{
              width: 40, height: 22, borderRadius: 11, cursor: 'pointer',
              background: status.takeover.active ? 'var(--success)' : '#333',
              position: 'relative', transition: 'background var(--transition)',
            }}
          >
            <div style={{
              width: 18, height: 18, borderRadius: 9,
              background: '#fff', position: 'absolute', top: 2,
              left: status.takeover.active ? 20 : 2,
              transition: 'left var(--transition)',
              boxShadow: '0 1px 3px rgba(0,0,0,0.3)',
            }} />
          </div>
        </div>

        {/* Codex Takeover */}
        <div style={{
          flex: 1, display: 'flex', justifyContent: 'space-between', alignItems: 'center',
          padding: '10px 16px',
          background: status.codex_takeover.active ? 'rgba(59,130,246,0.1)' : 'var(--bg-card)',
          border: `1px solid ${status.codex_takeover.active ? 'rgba(59,130,246,0.2)' : 'var(--border)'}`,
          borderRadius: 'var(--radius-md)',
        }}>
          <span style={{ fontSize: 13, color: status.codex_takeover.active ? 'var(--accent)' : 'var(--text-secondary)' }}>
            Codex
          </span>
          <div
            onClick={handleCodexTakeoverToggle}
            style={{
              width: 40, height: 22, borderRadius: 11, cursor: 'pointer',
              background: status.codex_takeover.active ? 'var(--accent)' : '#333',
              position: 'relative', transition: 'background var(--transition)',
            }}
          >
            <div style={{
              width: 18, height: 18, borderRadius: 9,
              background: '#fff', position: 'absolute', top: 2,
              left: status.codex_takeover.active ? 20 : 2,
              transition: 'left var(--transition)',
              boxShadow: '0 1px 3px rgba(0,0,0,0.3)',
            }} />
          </div>
        </div>
      </div>

      {/* Tabs */}
      <div style={{
        display: 'flex', gap: 0, marginBottom: 20,
        borderBottom: '1px solid var(--border)',
      }}>
        {tabs.map(t => (
          <button
            key={t.key}
            onClick={() => setTab(t.key)}
            style={{
              padding: '10px 20px', fontSize: 13, cursor: 'pointer',
              background: 'transparent', border: 'none',
              color: tab === t.key ? 'var(--text-primary)' : 'var(--text-muted)',
              borderBottom: tab === t.key ? '2px solid var(--accent)' : '2px solid transparent',
              fontWeight: tab === t.key ? 600 : 400,
              display: 'flex', alignItems: 'center', gap: 6,
            }}
          >
            <span style={{ fontSize: 14 }}>{t.icon}</span>
            {t.label}
          </button>
        ))}
      </div>

      {/* Content */}
      <div>
        {tab === 'logs' && <LogsPage />}
        {tab === 'providers' && <ProvidersPage config={config} onConfigChange={handleConfigChange} />}
        {tab === 'routes' && <RoutesPage config={config} onConfigChange={handleConfigChange} />}
        {tab === 'tags' && <TagsPage config={config} onConfigChange={handleConfigChange} />}
      </div>
    </div>
  );
}

export default App;
