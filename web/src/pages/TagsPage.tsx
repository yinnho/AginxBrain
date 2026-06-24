import { useState, useEffect } from 'react';
import type { AppConfig, Tag, Route, SmartRoutingConfig } from '../lib/api';
import * as api from '../lib/api';
import { Button } from '../components/Button';
import { Card } from '../components/Card';
import { Badge } from '../components/Badge';
import { Input } from '../components/Input';

export function TagsPage({ config, onConfigChange }: { config: AppConfig; onConfigChange: (c: AppConfig) => void }) {
  const [adding, setAdding] = useState(false);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [smartDraft, setSmartDraft] = useState<SmartRoutingConfig>(config.smart_routing);

  useEffect(() => {
    setSmartDraft(config.smart_routing);
  }, [config.smart_routing]);

  const toggleExpand = (name: string) => {
    setExpanded(prev => {
      const next = new Set(prev);
      if (next.has(name)) next.delete(name); else next.add(name);
      return next;
    });
  };

  const handleDelete = async (name: string) => {
    try {
      await api.deleteTag(name);
      const newTags = config.tags.filter(t => t.name !== name);
      onConfigChange({ ...config, tags: newTags });
    } catch (e: any) {
      alert(e.message || 'Failed to delete tag');
    }
  };

  const handlePriorityMove = async (tagName: string, routeId: string, direction: 'up' | 'down') => {
    // Find routes associated with this tag, sorted by current priority
    const tagRoutes = getTagRoutes(config.routes, tagName, config.tags.find(t => t.name === tagName)?.route_priority || {});
    if (tagRoutes.length < 2) return;

    const currentPos = tagRoutes.findIndex(r => r.route.id === routeId);
    if (currentPos < 0) return;
    const swapPos = direction === 'up' ? currentPos - 1 : currentPos + 1;
    if (swapPos < 0 || swapPos >= tagRoutes.length) return;

    // Build new route_priority: assign sequential priorities in the new order
    const tag = config.tags.find(t => t.name === tagName);
    if (!tag) return;

    const newOrder = [...tagRoutes];
    [newOrder[currentPos], newOrder[swapPos]] = [newOrder[swapPos], newOrder[currentPos]];

    const newPriority: Record<string, number> = {};
    newOrder.forEach((r, i) => {
      newPriority[r.route.id] = i;
    });

    try {
      await api.patchTag(tagName, { route_priority: newPriority });
      const newTags = config.tags.map(t =>
        t.name === tagName ? { ...t, route_priority: newPriority } : t
      );
      onConfigChange({ ...config, tags: newTags });
    } catch (e: any) {
      alert(e.message || 'Failed to update tag priority');
    }
  };

  const handleSmartRoutingSave = async (sr: SmartRoutingConfig) => {
    try {
      await api.updateConfig({ ...config, smart_routing: sr });
      onConfigChange({ ...config, smart_routing: sr });
    } catch (e: any) {
      alert(e.message || 'Failed to save smart routing config');
    }
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
            try {
              await api.createTag(tag);
              onConfigChange({ ...config, tags: [...config.tags, tag] });
              setAdding(false);
            } catch (e: any) {
              alert(e.message || 'Failed to create tag');
            }
          }}
          onCancel={() => setAdding(false)}
        />
      )}

      <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
        {config.tags.map(tag => {
          const routeCount = config.routes.filter(r => r.tags.includes(tag.name)).length;
          const isExpanded = expanded.has(tag.name);
          const tagRoutes = getTagRoutes(config.routes, tag.name, tag.route_priority || {});

          return (
            <Card key={tag.name} style={{ padding: 0, overflow: 'hidden' }}>
              {/* Header row */}
              <div
                style={{
                  display: 'flex', justifyContent: 'space-between', alignItems: 'center',
                  padding: '12px 16px', cursor: routeCount > 0 ? 'pointer' : 'default',
                }}
                onClick={() => routeCount > 0 && toggleExpand(tag.name)}
              >
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
                <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                  {routeCount > 0 && (
                    <span style={{ fontSize: 14, color: 'var(--text-muted)', transition: 'transform 0.2s', transform: isExpanded ? 'rotate(180deg)' : 'rotate(0deg)' }}>
                      ▼
                    </span>
                  )}
                  {!tag.is_auto && (
                    <Button variant="danger" onClick={(e: React.MouseEvent) => { e.stopPropagation(); handleDelete(tag.name); }} style={{ fontSize: 12, padding: '3px 10px' }}>Delete</Button>
                  )}
                </div>
              </div>

	              {/* Expandable route priority list + smart routing config (auto tag only) */}
              {isExpanded && (
                <div style={{ borderTop: '1px solid var(--border)', padding: '8px 16px 12px' }}>
                  {tagRoutes.length > 0 && tagRoutes.map((r, i) => {
                    return (
                      <div key={r.route.id} style={{
                        display: 'flex', alignItems: 'center', gap: 8,
                        padding: '6px 0',
                        borderTop: i > 0 ? '1px solid var(--border)' : 'none',
                      }}>
                        <span style={{ fontSize: 11, color: 'var(--text-muted)', width: 18, textAlign: 'center' }}>
                          {i + 1}
                        </span>
                        <div style={{ display: 'flex', flexDirection: 'column', gap: 2, flex: 1 }}>
                          <span style={{ fontSize: 13, fontWeight: 500 }}>{r.route.model}</span>
                          <span style={{ fontSize: 11, color: 'var(--text-muted)' }}>
                            {config.providers[r.route.provider]?.name || r.route.provider}
                          </span>
                        </div>
                        <div style={{ display: 'flex', flexDirection: 'column', gap: 2 }}>
                          <button
                            disabled={i === 0}
                            onClick={() => handlePriorityMove(tag.name, r.route.id, 'up')}
                            style={{ background: 'var(--bg-input)', border: '1px solid var(--border)', color: 'var(--text-secondary)', borderRadius: 3, padding: '0 6px', cursor: i === 0 ? 'default' : 'pointer', fontSize: 10, lineHeight: '16px', opacity: i === 0 ? 0.3 : 1 }}
                          >▲</button>
                          <button
                            disabled={i === tagRoutes.length - 1}
                            onClick={() => handlePriorityMove(tag.name, r.route.id, 'down')}
                            style={{ background: 'var(--bg-input)', border: '1px solid var(--border)', color: 'var(--text-secondary)', borderRadius: 3, padding: '0 6px', cursor: i === tagRoutes.length - 1 ? 'default' : 'pointer', fontSize: 10, lineHeight: '16px', opacity: i === tagRoutes.length - 1 ? 0.3 : 1 }}
                          >▼</button>
                        </div>
                      </div>
                    );
                  })}

                  {/* Smart Routing config — only for auto tag */}
                  {tag.is_auto && (
                    <div style={{ marginTop: 12, borderTop: tagRoutes.length > 0 ? '1px solid var(--border)' : 'none', paddingTop: 12 }}>
                      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 8 }}>
                        <span style={{ fontSize: 12, fontWeight: 600, color: 'var(--text-secondary)' }}>
                          🧠 Smart Routing
                        </span>
                        <label style={{ display: 'flex', alignItems: 'center', gap: 6, fontSize: 12, cursor: 'pointer' }}>
                          <input
                            type="checkbox"
                            checked={smartDraft.enabled}
                            onChange={e => {
                              const next = { ...smartDraft, enabled: e.target.checked };
                              setSmartDraft(next);
                              handleSmartRoutingSave(next);
                            }}
                          />
                          <span style={{ color: smartDraft.enabled ? 'var(--success)' : 'var(--text-muted)' }}>
                            {smartDraft.enabled ? 'Enabled' : 'Disabled'}
                          </span>
                        </label>
                      </div>

                      {smartDraft.enabled && (
                        <>
                          {/* Signal → Tier mapping */}
                          <div style={{ fontSize: 11, color: 'var(--text-muted)', marginBottom: 6 }}>
                            Signal → Tier mapping
                          </div>
                          <div style={{ display: 'flex', flexDirection: 'column', gap: 4, marginBottom: 10 }}>
                            {Object.entries(smartDraft.signal_tiers).sort(([a], [b]) => a.localeCompare(b)).map(([signal, tier]) => (
                              <div key={signal} style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                                <span style={{ fontSize: 12, width: 120, color: 'var(--text-secondary)', fontFamily: 'monospace' }}>{signal}</span>
                                <select
                                  value={tier}
                                  onChange={e => {
                                    const next = { ...smartDraft, signal_tiers: { ...smartDraft.signal_tiers, [signal]: e.target.value } };
                                    setSmartDraft(next);
                                  }}
                                  style={{
                                    fontSize: 12, padding: '2px 6px', borderRadius: 'var(--radius-sm)',
                                    background: 'var(--bg-input)', border: '1px solid var(--border)', color: 'var(--text-primary)',
                                  }}
                                >
                                  <option value="haiku">haiku</option>
                                  <option value="sonnet">sonnet</option>
                                  <option value="opus">opus</option>
                                </select>
                              </div>
                            ))}
                          </div>

                          {/* Advanced settings */}
                          <details style={{ fontSize: 11, color: 'var(--text-muted)' }}>
                            <summary style={{ cursor: 'pointer', marginBottom: 4 }}>Advanced settings</summary>
                            <div style={{ display: 'flex', gap: 12, alignItems: 'center', padding: '6px 0' }}>
                              <label style={{ display: 'flex', alignItems: 'center', gap: 4 }}>
                                Cache TTL (s)
                                <input
                                  type="number"
                                  value={smartDraft.cache_ttl_secs}
                                  onChange={e => setSmartDraft({ ...smartDraft, cache_ttl_secs: Number(e.target.value) })}
                                  style={{ width: 64, fontSize: 11, padding: '2px 6px', borderRadius: 'var(--radius-sm)', background: 'var(--bg-input)', border: '1px solid var(--border)', color: 'var(--text-primary)' }}
                                />
                              </label>
                              <label style={{ display: 'flex', alignItems: 'center', gap: 4 }}>
                                Max sessions
                                <input
                                  type="number"
                                  value={smartDraft.cache_max_sessions}
                                  onChange={e => setSmartDraft({ ...smartDraft, cache_max_sessions: Number(e.target.value) })}
                                  style={{ width: 64, fontSize: 11, padding: '2px 6px', borderRadius: 'var(--radius-sm)', background: 'var(--bg-input)', border: '1px solid var(--border)', color: 'var(--text-primary)' }}
                                />
                              </label>
                            </div>
                          </details>

                          {/* Save button for signal tier / advanced changes */}
                          <div style={{ marginTop: 8 }}>
                            <Button
                              variant="primary"
                              onClick={() => handleSmartRoutingSave(smartDraft)}
                              style={{ fontSize: 11, padding: '3px 12px' }}
                            >
                              Save Smart Routing
                            </Button>
                          </div>
                        </>
                      )}
                    </div>
                  )}
                </div>
              )}
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

/** Get routes associated with a tag, sorted by route_priority (keyed by route ID). */
function getTagRoutes(routes: Route[], tagName: string, tagPriority?: Record<string, number> | null): { route: Route }[] {
  const matched = routes
    .filter(route => route.tags.includes(tagName));

  if (tagPriority && Object.keys(tagPriority).length > 0) {
    matched.sort((a, b) => {
      const pa = tagPriority[a.id] ?? Infinity;
      const pb = tagPriority[b.id] ?? Infinity;
      return pa - pb;
    });
  }
  return matched.map(route => ({ route }));
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
        <Button variant="primary" disabled={!name} onClick={() => onSave({ name, color, is_auto: false, route_priority: {} })}>Save</Button>
        <Button variant="ghost" onClick={onCancel}>Cancel</Button>
      </div>
    </Card>
  );
}
