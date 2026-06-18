import { useEffect, useState } from 'react';
import * as api from '../lib/api';
import { UsageGuide } from '../components/UsageGuide';

export function ApiKeysPage() {
  const [keys, setKeys] = useState<api.CallerKey[]>([]);
  const [loading, setLoading] = useState(true);
  const [name, setName] = useState('');
  const [note, setNote] = useState('');
  const [newToken, setNewToken] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = async () => {
    try {
      const list = await api.listKeys();
      setKeys(list);
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to load keys');
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    refresh();
  }, []);

  const handleCreate = async () => {
    if (!name.trim()) return;
    try {
      const created = await api.createKey(name.trim(), note.trim());
      setNewToken(created.token);
      setName('');
      setNote('');
      await refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to create key');
    }
  };

  const toggleEnabled = async (key: api.CallerKey) => {
    try {
      await api.updateKey({ ...key, enabled: !key.enabled });
      await refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to update key');
    }
  };

  const handleDelete = async (id: number) => {
    if (!confirm('Delete this API key? It cannot be undone.')) return;
    try {
      await api.deleteKey(id);
      await refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to delete key');
    }
  };

  return (
    <div>
      <h3 style={{ margin: '0 0 16px', fontSize: 16 }}>API Keys</h3>
      {error && <div style={{ color: '#EF4444', fontSize: 13, marginBottom: 12 }}>{error}</div>}

      <div style={{
        display: 'grid', gridTemplateColumns: '1fr 1.5fr auto auto', gap: 8, marginBottom: 16,
        alignItems: 'end',
      }}>
        <div>
          <label style={{ fontSize: 12, color: 'var(--text-muted)' }}>Name</label>
          <input
            type="text"
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="e.g. Team A"
            style={{
              width: '100%', padding: '8px 10px', fontSize: 13,
              border: '1px solid var(--border)', borderRadius: 6,
              background: 'var(--bg-primary)', color: 'var(--text-primary)',
              boxSizing: 'border-box',
            }}
          />
        </div>
        <div>
          <label style={{ fontSize: 12, color: 'var(--text-muted)' }}>Note</label>
          <input
            type="text"
            value={note}
            onChange={(e) => setNote(e.target.value)}
            placeholder="Optional note"
            style={{
              width: '100%', padding: '8px 10px', fontSize: 13,
              border: '1px solid var(--border)', borderRadius: 6,
              background: 'var(--bg-primary)', color: 'var(--text-primary)',
              boxSizing: 'border-box',
            }}
          />
        </div>
        <button
          onClick={handleCreate}
          disabled={!name.trim()}
          style={{
            padding: '8px 14px', fontSize: 13, fontWeight: 600,
            background: 'var(--accent)', color: '#fff', border: 'none',
            borderRadius: 6, cursor: 'pointer', opacity: !name.trim() ? 0.5 : 1,
          }}
        >Create Key</button>
      </div>

      {newToken && (
        <div style={{
          background: 'rgba(34, 197, 94, 0.08)', border: '1px solid rgba(34, 197, 94, 0.3)',
          borderRadius: 8, padding: 16, marginBottom: 16, fontSize: 13,
        }}>
          <div style={{ marginBottom: 6, fontWeight: 600 }}>✓ API key 已创建 — 请立即复制，之后不再显示</div>
          <div style={{
            fontFamily: 'monospace', background: 'var(--bg-primary)', padding: 8,
            borderRadius: 6, wordBreak: 'break-all', userSelect: 'all', marginBottom: 14,
          }}>
            {newToken}
          </div>
          <div style={{ borderTop: '1px solid var(--border)', paddingTop: 12 }}>
            <UsageGuide token={newToken} />
          </div>
          <button
            onClick={() => setNewToken(null)}
            style={{
              marginTop: 8, background: 'transparent', border: '1px solid var(--border)',
              borderRadius: 6, padding: '4px 10px', fontSize: 12, cursor: 'pointer',
            }}
          >知道了</button>
        </div>
      )}

      {loading ? (
        <div style={{ color: 'var(--text-muted)', fontSize: 13 }}>Loading...</div>
      ) : (
        <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: 13 }}>
          <thead>
            <tr style={{ borderBottom: '1px solid var(--border)' }}>
              <th style={{ textAlign: 'left', padding: '8px 0', color: 'var(--text-muted)', fontWeight: 500 }}>Name</th>
              <th style={{ textAlign: 'left', padding: '8px 0', color: 'var(--text-muted)', fontWeight: 500 }}>API Key</th>
              <th style={{ textAlign: 'left', padding: '8px 0', color: 'var(--text-muted)', fontWeight: 500 }}>Note</th>
              <th style={{ textAlign: 'left', padding: '8px 0', color: 'var(--text-muted)', fontWeight: 500 }}>Status</th>
              <th style={{ textAlign: 'left', padding: '8px 0', color: 'var(--text-muted)', fontWeight: 500 }}>Created</th>
              <th style={{ textAlign: 'right', padding: '8px 0', color: 'var(--text-muted)', fontWeight: 500 }}>Actions</th>
            </tr>
          </thead>
          <tbody>
            {keys.map((key) => (
              <KeyRow key={key.id} keyData={key} onDelete={handleDelete} onToggle={toggleEnabled} />
            ))}
            {keys.length === 0 && (
              <tr>
                <td colSpan={6} style={{ padding: 20, textAlign: 'center', color: 'var(--text-muted)' }}>
                  No API keys yet.
                </td>
              </tr>
            )}
          </tbody>
        </table>
      )}

      {!newToken && (
        <div style={{
          marginTop: 32, padding: 16,
          background: 'var(--bg-card)', border: '1px solid var(--border)',
          borderRadius: 'var(--radius-md)',
        }}>
          <div style={{ fontSize: 14, fontWeight: 600, color: 'var(--text-primary)', marginBottom: 4 }}>
            使用方法
          </div>
          <div style={{ fontSize: 12, color: 'var(--text-muted)', marginBottom: 8 }}>
            用任意 key 替换下面的占位符。Base URL 是当前站点地址。
          </div>
          <UsageGuide token="<your-api-key>" />
        </div>
      )}
    </div>
  );
}

// One row in the keys table, with a reveal/copy control for the API key value.
function KeyRow({
  keyData,
  onDelete,
  onToggle,
}: {
  keyData: api.CallerKey;
  onDelete: (id: number) => void;
  onToggle: (key: api.CallerKey) => void;
}) {
  const [revealed, setRevealed] = useState(false);
  const [copied, setCopied] = useState(false);

  const token = keyData.token;
  const masked = token ? `${token.slice(0, 10)}…${token.slice(-4)}` : null;

  const copy = () => {
    if (!token) return;
    navigator.clipboard?.writeText(token).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    });
  };

  return (
    <tr style={{ borderBottom: '1px solid var(--border)' }}>
      <td style={{ padding: '10px 0' }}>{keyData.name}</td>
      <td style={{ padding: '10px 0' }}>
        {token ? (
          <div style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
            <span className="mono" style={{ fontSize: 12, color: 'var(--text-secondary)' }}>
              {revealed ? token : masked}
            </span>
            <button
              onClick={() => setRevealed((v) => !v)}
              style={{
                background: 'transparent', border: '1px solid var(--border)',
                borderRadius: 4, padding: '1px 6px', fontSize: 11,
                color: 'var(--text-muted)', cursor: 'pointer',
              }}
            >
              {revealed ? '隐藏' : '显示'}
            </button>
            <button
              onClick={copy}
              style={{
                background: 'transparent', border: '1px solid var(--border)',
                borderRadius: 4, padding: '1px 6px', fontSize: 11,
                color: copied ? 'var(--success, #22C55E)' : 'var(--accent, #3B82F6)', cursor: 'pointer',
              }}
            >
              {copied ? '已复制' : '复制'}
            </button>
          </div>
        ) : (
          <span style={{ fontSize: 12, color: 'var(--text-muted)' }}>
            仅创建时可见（旧 key）
          </span>
        )}
      </td>
      <td style={{ padding: '10px 0', color: 'var(--text-muted)' }}>{keyData.note || '-'}</td>
      <td style={{ padding: '10px 0' }}>
        <button
          onClick={() => onToggle(keyData)}
          style={{
            background: keyData.enabled ? 'rgba(34, 197, 94, 0.12)' : 'rgba(100, 116, 139, 0.12)',
            color: keyData.enabled ? '#22C55E' : 'var(--text-muted)',
            border: 'none', borderRadius: 12, padding: '3px 10px',
            fontSize: 12, cursor: 'pointer', fontWeight: 500,
          }}
        >
          {keyData.enabled ? 'Active' : 'Disabled'}
        </button>
      </td>
      <td style={{ padding: '10px 0', color: 'var(--text-muted)', fontSize: 12 }}>{keyData.created_at.slice(0, 10)}</td>
      <td style={{ padding: '10px 0', textAlign: 'right' }}>
        <button
          onClick={() => onDelete(keyData.id)}
          style={{
            background: 'transparent', border: '1px solid var(--border)',
            borderRadius: 6, padding: '4px 10px', fontSize: 12,
            color: '#EF4444', cursor: 'pointer',
          }}
        >Delete</button>
      </td>
    </tr>
  );
}
