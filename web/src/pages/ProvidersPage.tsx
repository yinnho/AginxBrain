import { useState } from 'react';
import type { AppConfig, Provider } from '../lib/api';
import * as api from '../lib/api';
import { Button } from '../components/Button';
import { Card } from '../components/Card';
import { Input, Select } from '../components/Input';

export function ProvidersPage({ config, onConfigChange }: { config: AppConfig; onConfigChange: (c: AppConfig) => void }) {
  const [editing, setEditing] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [showKeys, setShowKeys] = useState<Record<string, boolean>>({});

  const providerList = Object.entries(config.providers);

  const handleDelete = async (id: string) => {
    try {
      await api.deleteProvider(id);
      const newProviders = { ...config.providers };
      delete newProviders[id];
      onConfigChange({ ...config, providers: newProviders });
    } catch (e: any) {
      alert(e.message || 'Failed to delete provider');
    }
  };

  const toggleKeyVisibility = (id: string) => {
    setShowKeys(prev => ({ ...prev, [id]: !prev[id] }));
  };

  return (
    <div>
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 16 }}>
        <h2 style={{ margin: 0, fontSize: 16, fontWeight: 600 }}>Providers</h2>
        <Button variant="primary" onClick={() => setAdding(true)}>+ Add</Button>
      </div>

      {adding && (
        <ProviderForm
          initial={undefined}
          onSave={async (id, provider) => {
            try {
              await api.createProvider(id, provider);
              onConfigChange({ ...config, providers: { ...config.providers, [id]: provider } });
              setAdding(false);
            } catch (e: any) {
              alert(e.message || 'Failed to save provider');
            }
          }}
          onCancel={() => { setAdding(false); }}
        />
      )}

      <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
        {providerList.map(([id, p]) => (
          <div key={id} style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
          <Card style={{ padding: '12px 16px' }}>
            <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
              <div style={{ flex: 1, minWidth: 0 }}>
                <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                  <span style={{ fontWeight: 600 }}>{p.name}</span>
                  <span className="mono" style={{ fontSize: 11, color: 'var(--text-muted)' }}>{id}</span>
                </div>
                <div className="mono" style={{ fontSize: 12, color: 'var(--text-secondary)', marginTop: 2, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                  {p.base_url}
                </div>
                <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginTop: 4 }}>
                  <span style={{ fontSize: 11, color: 'var(--text-muted)' }}>{p.auth_type}</span>
                  <span className="mono" style={{ fontSize: 11, color: 'var(--text-muted)' }}>
                    {showKeys[id] ? p.api_key : `${p.api_key.slice(0, 8)}${'•'.repeat(8)}`}
                  </span>
                  <button onClick={() => toggleKeyVisibility(id)} style={{
                    fontSize: 11, background: 'none', border: 'none', color: 'var(--text-muted)', cursor: 'pointer', padding: '0 4px',
                  }}>
                    {showKeys[id] ? 'hide' : 'show'}
                  </button>
                </div>
              </div>
              <div style={{ display: 'flex', gap: 4, marginLeft: 12 }}>
                <Button variant="ghost" onClick={() => setEditing(id)} style={{ padding: '4px 10px' }}>Edit</Button>
                <Button variant="danger" onClick={() => handleDelete(id)} style={{ padding: '4px 10px' }}>Delete</Button>
              </div>
            </div>
          </Card>

          {editing === id && (
            <ProviderForm
              initial={{ id, ...p }}
              onSave={async (newId, provider) => {
                try {
                  const updated = await api.updateProvider(newId, provider);
                  const newProviders = { ...config.providers };
                  if (newId !== id) delete newProviders[id];
                  newProviders[newId] = updated;
                  onConfigChange({ ...config, providers: newProviders });
                  setEditing(null);
                } catch (e: any) {
                  alert(e.message || 'Failed to save provider');
                }
              }}
              onCancel={() => { setEditing(null); }}
            />
          )}
          </div>
        ))}
        {providerList.length === 0 && (
          <div style={{ padding: 48, textAlign: 'center', color: 'var(--text-muted)', border: '1px dashed var(--border)', borderRadius: 'var(--radius-md)' }}>
            <div style={{ fontSize: 28, marginBottom: 8 }}>🔌</div>
            No providers configured
          </div>
        )}
      </div>
    </div>
  );
}

function ProviderForm({ initial, onSave, onCancel }: {
  initial?: { id: string } & Provider;
  onSave: (id: string, provider: Provider) => void;
  onCancel: () => void;
}) {
  const [id, setId] = useState(initial?.id || '');
  const [name, setName] = useState(initial?.name || '');
  const [baseUrl, setBaseUrl] = useState(initial?.base_url || '');
  const [apiKey, setApiKey] = useState(initial?.api_key || '');
  const [authType, setAuthType] = useState(initial?.auth_type || 'bearer');

  const valid = id && name && baseUrl && apiKey;

  return (
    <Card style={{ marginBottom: 16, background: 'var(--bg-input)' }}>
      <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 12 }}>
        <Input label="ID" value={id} onChange={e => setId(e.target.value)} disabled={!!initial} />
        <Input label="Name" value={name} onChange={e => setName(e.target.value)} />
        <Input label="Base URL" value={baseUrl} onChange={e => setBaseUrl(e.target.value)} placeholder="https://api.deepseek.com" />
        <Select label="Auth Type" value={authType} onChange={e => setAuthType(e.target.value as any)}>
          <option value="bearer">Bearer</option>
          <option value="x_api_key">x-api-key</option>
          <option value="x_goog_api_key">x-goog-api-key</option>
        </Select>
        <div style={{ gridColumn: '1 / -1' }}>
          <Input label="API Key" value={apiKey} onChange={e => setApiKey(e.target.value)} type="password" />
        </div>
      </div>
      <div style={{ marginTop: 14, display: 'flex', gap: 8 }}>
        <Button variant="primary" disabled={!valid} onClick={() => onSave(id, { name, base_url: baseUrl, api_key: apiKey, auth_type: authType as any, ws_url: initial?.ws_url })}>Save</Button>
        <Button variant="ghost" onClick={onCancel}>Cancel</Button>
      </div>
    </Card>
  );
}
