import { useState } from 'react';
import type { AppConfig, Tag } from '../lib/api';
import * as api from '../lib/api';
import { Button } from '../components/Button';
import { Card } from '../components/Card';
import { Badge } from '../components/Badge';
import { Input } from '../components/Input';

export function TagsPage({ config, onConfigChange }: { config: AppConfig; onConfigChange: (c: AppConfig) => void }) {
  const [adding, setAdding] = useState(false);

  const handleDelete = async (name: string) => {
    const newTags = config.tags.filter(t => t.name !== name);
    const newConfig = { ...config, tags: newTags };
    await api.updateConfig(newConfig);
    onConfigChange(newConfig);
  };

  return (
    <div>
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 16 }}>
        <h2 style={{ margin: 0, fontSize: 16, fontWeight: 600 }}>Tags</h2>
        <Button variant="primary" onClick={() => setAdding(true)}>+ Add</Button>
      </div>

      {adding && (
        <TagForm
          onSave={async (tag) => {
            const newConfig = { ...config, tags: [...config.tags, tag] };
            await api.updateConfig(newConfig);
            onConfigChange(newConfig);
            setAdding(false);
          }}
          onCancel={() => setAdding(false)}
        />
      )}

      <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
        {config.tags.map(tag => {
          const routeCount = config.routes.filter(r => r.tags.includes(tag.name)).length;
          return (
            <Card key={tag.name} style={{ padding: '12px 16px' }}>
              <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
                <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
                  <div style={{
                    width: 4, height: 28, borderRadius: 2,
                    background: tag.color || '#555',
                  }} />
                  <Badge color={tag.color || '#555'} style={{ fontSize: 14, padding: '4px 14px' }}>{tag.name}</Badge>
                  {tag.is_auto && (
                    <span style={{
                      fontSize: 10, fontWeight: 700, color: 'var(--warning)',
                      padding: '1px 6px', borderRadius: 'var(--radius-sm)',
                      background: 'rgba(245,158,11,0.12)', letterSpacing: '0.5px',
                    }}>AUTO</span>
                  )}
                  <span style={{ fontSize: 12, color: 'var(--text-muted)' }}>
                    {routeCount} route{routeCount !== 1 ? 's' : ''}
                  </span>
                </div>
                {!tag.is_auto && (
                  <Button variant="danger" onClick={() => handleDelete(tag.name)} style={{ fontSize: 12, padding: '3px 10px' }}>Delete</Button>
                )}
              </div>
            </Card>
          );
        })}
        {config.tags.length === 0 && (
          <div style={{ padding: 48, textAlign: 'center', color: 'var(--text-muted)', border: '1px dashed var(--border)', borderRadius: 'var(--radius-md)' }}>
            <div style={{ fontSize: 28, marginBottom: 8 }}>🏷️</div>
            No tags configured
          </div>
        )}
      </div>
    </div>
  );
}

function TagForm({ onSave, onCancel }: { onSave: (tag: Tag) => void; onCancel: () => void }) {
  const [name, setName] = useState('');
  const [color, setColor] = useState('#3B82F6');

  return (
    <Card style={{ marginBottom: 16, background: 'var(--bg-input)' }}>
      <div style={{ display: 'flex', gap: 12, alignItems: 'flex-end' }}>
        <div style={{ flex: 1 }}>
          <Input label="Name" value={name} onChange={e => setName(e.target.value)} placeholder="e.g. sonnet" />
        </div>
        <div>
          <label style={{ display: 'block', fontSize: 12, color: 'var(--text-secondary)', marginBottom: 4, fontWeight: 500 }}>Color</label>
          <input type="color" value={color} onChange={e => setColor(e.target.value)}
            style={{ width: 40, height: 36, padding: 2, cursor: 'pointer', background: 'none', border: '1px solid var(--border)', borderRadius: 'var(--radius-sm)' }} />
        </div>
        <Button variant="primary" disabled={!name} onClick={() => onSave({ name, color, is_auto: false })}>Save</Button>
        <Button variant="ghost" onClick={onCancel}>Cancel</Button>
      </div>
    </Card>
  );
}
