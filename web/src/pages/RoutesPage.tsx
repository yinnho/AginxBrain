import { useState } from 'react';
import type { AppConfig, GenerateImageResponse, Route, RouteFormat, TestResult } from '../lib/api';
import { FORMAT_ENDPOINTS } from '../lib/api';
import * as api from '../lib/api';
import { Button } from '../components/Button';
import { Card } from '../components/Card';
import { Badge } from '../components/Badge';
import { Input, Select } from '../components/Input';

export function RoutesPage({ config, onConfigChange }: { config: AppConfig; onConfigChange: (c: AppConfig) => void }) {
  const [editing, setEditing] = useState<number | null>(null);
  const [adding, setAdding] = useState(false);
  const [testResults, setTestResults] = useState<Record<number, TestResult>>({});
  const [testing, setTesting] = useState<number | null>(null);
  const [imageTag, setImageTag] = useState(config.current_tag || config.tags[0]?.name || 'image');
  const [imagePrompt, setImagePrompt] = useState('');
  const [imageSize, setImageSize] = useState('1024x1024');
  const [imageCount, setImageCount] = useState(1);
  const [imageLoading, setImageLoading] = useState(false);
  const [imageResult, setImageResult] = useState<GenerateImageResponse | null>(null);

  const handleDelete = async (index: number) => {
    try {
      await api.deleteRoute(index);
      const newRoutes = config.routes.filter((_, i) => i !== index);
      onConfigChange({ ...config, routes: newRoutes });
    } catch (e: any) {
      alert(e.message || 'Failed to delete route');
    }
  };

  const handleTest = async (index: number) => {
    setTesting(index);
    try {
      const result = await api.testRouteByIndex(index);
      setTestResults(prev => ({ ...prev, [index]: result }));
    } catch (e: any) {
      setTestResults(prev => ({ ...prev, [index]: { success: false, tag: '', provider: '', model: '', format: '', latency_ms: 0, error: e.message, response: null } }));
    }
    setTesting(null);
  };

  const handleToggleEnabled = async (index: number) => {
    try {
      const updated = await api.patchRoute(index, { enabled: !config.routes[index].enabled });
      const newRoutes = config.routes.map((r, i) => i === index ? updated : r);
      onConfigChange({ ...config, routes: newRoutes });
    } catch (e: any) {
      alert(e.message || 'Failed to toggle route');
    }
  };

  const handleMove = async (index: number, direction: -1 | 1) => {
    const target = index + direction;
    if (target < 0 || target >= config.routes.length) return;
    try {
      await api.moveRoute(index, direction);
      const newRoutes = [...config.routes];
      [newRoutes[index], newRoutes[target]] = [newRoutes[target], newRoutes[index]];
      onConfigChange({ ...config, routes: newRoutes });
    } catch (e: any) {
      alert(e.message || 'Failed to move route');
    }
  };

  const handleGenerateImage = async () => {
    setImageLoading(true);
    setImageResult(null);
    try {
      const result = await api.generateImage({
        tag: imageTag,
        prompt: imagePrompt,
        size: imageSize,
        n: imageCount,
      });
      setImageResult(result);
    } catch (e: any) {
      setImageResult({
        success: false,
        tag: imageTag,
        provider: '',
        model: '',
        format: '',
        images: [],
        latency_ms: 0,
        error: e.message || 'Image generation failed',
      });
    }
    setImageLoading(false);
  };

  /** Format badge color based on format type. */
  const formatColor = (fmt: RouteFormat) => {
    if (fmt === 'openai') return '#10a37f';
    if (fmt === 'openai_responses') return '#6366f1';
    if (fmt.includes('image') || fmt === 'dashscope_chat_image') return '#ec4899';
    if (fmt.includes('video') || fmt === 'kling') return '#8b5cf6';
    if (fmt.includes('tts')) return '#f97316';
    if (fmt.includes('asr')) return '#06B6D4';
    return '#d97706';
  };

  return (
    <div>
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 16 }}>
        <h2 style={{ margin: 0, fontSize: 16, fontWeight: 600 }}>Routes</h2>
        <Button variant="primary" onClick={() => setAdding(true)}>+ Add</Button>
      </div>

      {adding && (
        <RouteForm
          initial={undefined}
          providers={Object.keys(config.providers)}
          tags={config.tags.map(t => t.name)}
          onSave={async (route) => {
            try {
              await api.createRoute(route);
              onConfigChange({ ...config, routes: [...config.routes, route] });
              setAdding(false);
            } catch (e: any) {
              alert(e.message || 'Failed to save route');
            }
          }}
          onCancel={() => { setAdding(false); }}
        />
      )}

      <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
        {config.routes.map((route, i) => {
          const tr = testResults[i];
          return (
            <div key={i} style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
            <Card style={{ opacity: route.enabled ? 1 : 0.45, transition: 'opacity 0.2s' }}>
              <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'flex-start' }}>
                <div style={{ display: 'flex', gap: 8, alignItems: 'flex-start' }}>
                  {/* Ordering buttons */}
                  <div style={{ display: 'flex', flexDirection: 'column', gap: 2, marginTop: 2 }}>
                    <button
                      disabled={i === 0}
                      onClick={() => handleMove(i, -1)}
                      style={{
                        background: 'transparent', border: 'none', color: 'var(--text-muted)',
                        cursor: i === 0 ? 'default' : 'pointer', fontSize: 10, padding: 2,
                        opacity: i === 0 ? 0.3 : 1, lineHeight: 1,
                      }}
                      title="Move up"
                    >▲</button>
                    <button
                      disabled={i === config.routes.length - 1}
                      onClick={() => handleMove(i, 1)}
                      style={{
                        background: 'transparent', border: 'none', color: 'var(--text-muted)',
                        cursor: i === config.routes.length - 1 ? 'default' : 'pointer', fontSize: 10, padding: 2,
                        opacity: i === config.routes.length - 1 ? 0.3 : 1, lineHeight: 1,
                      }}
                      title="Move down"
                    >▼</button>
                  </div>
                  {/* Route info */}
                  <div>
                    <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                      <span style={{ fontSize: 15, fontWeight: 600 }}>{route.model}</span>
                      <Badge color={formatColor(route.format)}>{route.format || 'openai'}</Badge>
                    </div>
                    <div style={{ fontSize: 12, color: 'var(--text-muted)', marginTop: 4 }}>
                      via <span style={{ color: 'var(--text-secondary)' }}>{route.provider}</span>
                      <span className="mono" style={{ marginLeft: 8 }}>{route.endpoint}</span>
                    </div>
                    <div style={{ display: 'flex', gap: 4, marginTop: 8 }}>
                      {route.tags.map(t => {
                        const tagDef = config.tags.find(td => td.name === t);
                        return <Badge key={t} color={tagDef?.color || '#555'}>{t}</Badge>;
                      })}
                    </div>
                  </div>
                </div>
                <div style={{ display: 'flex', gap: 6, alignItems: 'center' }}>
                  {/* Enable/disable toggle */}
                  <div
                    onClick={() => handleToggleEnabled(i)}
                    title={route.enabled ? 'Disable route' : 'Enable route'}
                    style={{
                      width: 36, height: 20, borderRadius: 10, cursor: 'pointer',
                      background: route.enabled ? 'var(--success)' : '#444',
                      position: 'relative', transition: 'background 0.2s',
                    }}
                  >
                    <div style={{
                      width: 16, height: 16, borderRadius: 8,
                      background: '#fff', position: 'absolute', top: 2,
                      left: route.enabled ? 18 : 2,
                      transition: 'left 0.2s',
                      boxShadow: '0 1px 3px rgba(0,0,0,0.3)',
                    }} />
                  </div>
                  <Button
                    variant={testing === i ? 'secondary' : 'success'}
                    onClick={() => handleTest(i)}
                    disabled={testing !== null || !route.enabled}
                    style={{ fontSize: 12, padding: '4px 12px' }}
                  >
                    {testing === i ? '⏳ Testing...' : '▶ Test'}
                  </Button>
                  <Button variant="ghost" onClick={() => setEditing(i)} style={{ fontSize: 12, padding: '4px 10px' }}>Edit</Button>
                  <Button variant="danger" onClick={() => handleDelete(i)} style={{ fontSize: 12, padding: '4px 10px' }}>Delete</Button>
                </div>
              </div>

              {tr && (
                <div style={{
                  marginTop: 12, padding: 10, borderRadius: 'var(--radius-sm)', fontSize: 12,
                  background: tr.success ? 'var(--success-dim)' : 'var(--danger-dim)',
                  border: `1px solid ${tr.success ? 'rgba(34,197,94,0.2)' : 'rgba(239,68,68,0.2)'}`,
                }}>
                  {tr.success ? (
                    <div>
                      <span style={{ color: 'var(--success)', fontWeight: 600 }}>✓ OK</span>
                      <span style={{ color: 'var(--text-muted)', marginLeft: 8 }}>{tr.provider} / {tr.model}</span>
                      <span style={{ color: 'var(--text-muted)', marginLeft: 8 }}>{tr.latency_ms}ms</span>
                      {tr.response && (
                        <pre className="mono" style={{ marginTop: 6, color: 'var(--text-secondary)', whiteSpace: 'pre-wrap', maxHeight: 100, overflow: 'auto', fontSize: 11, background: 'rgba(0,0,0,0.2)', padding: 8, borderRadius: 'var(--radius-sm)' }}>
                          {(() => {
                            const content = tr.response?.content;
                            if (Array.isArray(content)) {
                              const textBlock = content.find((b: any) => b.type === 'text');
                              if (textBlock) return textBlock.text;
                              return JSON.stringify(content, null, 2);
                            }
                            return JSON.stringify(tr.response, null, 2);
                          })()}
                        </pre>
                      )}
                    </div>
                  ) : (
                    <div style={{ color: 'var(--danger)' }}>✗ {tr.error}</div>
                  )}
                </div>
              )}
            </Card>

            {editing === i && (
              <RouteForm
                initial={route}
                providers={Object.keys(config.providers)}
                tags={config.tags.map(t => t.name)}
                onSave={async (newRoute) => {
                  try {
                    const updated = await api.updateRoute(i, newRoute);
                    const newRoutes = config.routes.map((r, idx) => idx === i ? updated : r);
                    onConfigChange({ ...config, routes: newRoutes });
                    setEditing(null);
                  } catch (e: any) {
                    alert(e.message || 'Failed to save route');
                  }
                }}
                onCancel={() => { setEditing(null); }}
              />
            )}
            </div>
          );
        })}
        {config.routes.length === 0 && (
          <div style={{ padding: 48, textAlign: 'center', color: 'var(--text-muted)', border: '1px dashed var(--border)', borderRadius: 'var(--radius-md)' }}>
            <div style={{ fontSize: 28, marginBottom: 8 }}>🔀</div>
            No routes configured
          </div>
        )}
      </div>

      <Card style={{ marginTop: 20, background: 'rgba(236,72,153,0.06)', border: '1px solid rgba(236,72,153,0.18)' }}>
        <h3 style={{ margin: '0 0 12px 0', fontSize: 12, color: 'var(--text-muted)', fontWeight: 500, textTransform: 'uppercase', letterSpacing: '0.5px' }}>Generate Image</h3>
        <div style={{ display: 'grid', gridTemplateColumns: '160px 1fr 140px 90px', gap: 10, alignItems: 'end' }}>
          <Select label="Tag" value={imageTag} onChange={e => setImageTag(e.target.value)}>
            {config.tags.map(t => <option key={t.name} value={t.name}>{t.name}</option>)}
          </Select>
          <Input label="Prompt" value={imagePrompt} onChange={e => setImagePrompt(e.target.value)} placeholder="A cyberpunk cat at night" />
          <Input label="Size" value={imageSize} onChange={e => setImageSize(e.target.value)} placeholder="1024x1024" />
          <Input label="N" type="number" min="1" max="10" value={imageCount} onChange={e => setImageCount(Math.max(1, Number(e.target.value) || 1))} />
        </div>
        <div style={{ marginTop: 12, display: 'flex', gap: 8, alignItems: 'center' }}>
          <Button variant="primary" disabled={imageLoading || !imagePrompt.trim()} onClick={handleGenerateImage}>
            {imageLoading ? '⏳ Generating...' : '🎨 Generate'}
          </Button>
          <span style={{ fontSize: 12, color: 'var(--text-muted)' }}>
            Uses routes with image formats.
          </span>
        </div>
        {imageResult && (
          <div style={{ marginTop: 12, padding: 10, borderRadius: 'var(--radius-sm)', fontSize: 12, background: imageResult.success ? 'var(--success-dim)' : 'var(--danger-dim)' }}>
            {imageResult.success ? (
              <div>
                <div>
                  <span style={{ color: 'var(--success)', fontWeight: 600 }}>✓ Generated</span>
                  <span style={{ color: 'var(--text-muted)', marginLeft: 8 }}>{imageResult.provider} / {imageResult.model}</span>
                  <span style={{ color: 'var(--text-muted)', marginLeft: 8 }}>{imageResult.latency_ms}ms</span>
                </div>
                <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(160px, 1fr))', gap: 10, marginTop: 10 }}>
                  {imageResult.images.map((img, i) => {
                    const src = img.url || (img.base64 ? `data:image/png;base64,${img.base64}` : '');
                    return src ? (
                      <a key={i} href={src} target="_blank" rel="noreferrer" style={{ display: 'block' }}>
                        <img src={src} style={{ width: '100%', borderRadius: 'var(--radius-sm)', border: '1px solid var(--border)' }} />
                      </a>
                    ) : null;
                  })}
                </div>
              </div>
            ) : (
              <div style={{ color: 'var(--danger)' }}>✗ {imageResult.error}</div>
            )}
          </div>
        )}
      </Card>

      <Card style={{ marginTop: 20, background: 'rgba(0,0,0,0.2)' }}>
        <h3 style={{ margin: '0 0 8px 0', fontSize: 12, color: 'var(--text-muted)', fontWeight: 500, textTransform: 'uppercase', letterSpacing: '0.5px' }}>curl test commands</h3>
        <pre className="mono" style={{ fontSize: 11, color: 'var(--text-secondary)', whiteSpace: 'pre-wrap', lineHeight: 1.7 }}>
{`# Test a specific route
	curl -s http://127.0.0.1:${config.port}/api/test \\
	  -H 'Content-Type: application/json' \\
	  -d '{"tag":"haiku","prompt":"Say hello"}' | jq .

	# Test as Anthropic client
	curl -s http://127.0.0.1:${config.port}/anthropic/v1/messages \\
	  -H 'Content-Type: application/json' \\
	  -H 'x-api-key: test' \\
	  -d '{"model":"sonnet","max_tokens":64,"messages":[{"role":"user","content":"Hi"}]}' | jq .`}
        </pre>
      </Card>
    </div>
  );
}

function RouteForm({ initial, providers, tags, onSave, onCancel }: {
  initial?: Route;
  providers: string[];
  tags: string[];
  onSave: (route: Route) => void;
  onCancel: () => void;
}) {
  const [endpoint, setEndpoint] = useState(initial?.endpoint || '/v1/chat/completions');
  const [model, setModel] = useState(initial?.model || '');
  const [provider, setProvider] = useState(initial?.provider || providers[0] || '');
  const [selectedTags, setSelectedTags] = useState<string[]>(initial?.tags || []);
  const [format, setFormat] = useState<RouteFormat>(initial?.format || 'openai');

  const toggleTag = (tag: string) => {
    setSelectedTags(prev => prev.includes(tag) ? prev.filter(t => t !== tag) : [...prev, tag]);
  };

  const handleFormatChange = (newFormat: RouteFormat) => {
    setFormat(newFormat);
    setEndpoint(FORMAT_ENDPOINTS[newFormat] || '/v1/chat/completions');
  };

  const valid = endpoint && model && provider && selectedTags.length > 0;

  return (
    <Card style={{ marginBottom: 16, background: 'var(--bg-input)' }}>
      <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 12 }}>
        <Input label="Endpoint" value={endpoint} onChange={e => setEndpoint(e.target.value)} />
        <Input label="Model" value={model} onChange={e => setModel(e.target.value)} />
        <Select label="Provider" value={provider} onChange={e => setProvider(e.target.value)}>
          {providers.map(p => <option key={p} value={p}>{p}</option>)}
        </Select>
        <Select label="Format" value={format} onChange={e => handleFormatChange(e.target.value as RouteFormat)}>
          <option value="openai">OpenAI</option>
          <option value="anthropic">Anthropic</option>
          <option value="openai_responses">OpenAI Responses</option>
          <option value="openai_images">OpenAI Images</option>
          <option value="dashscope_image">DashScope Image</option>
          <option value="dashscope_chat_image">DashScope Chat Image</option>
          <option value="dashscope_video">DashScope Video</option>
          <option value="dashscope_tts">DashScope TTS</option>
          <option value="dashscope_asr">DashScope ASR</option>
          <option value="kling">Kling</option>
          <option value="minimax_image">MiniMax Image</option>
        </Select>
        <div style={{ gridColumn: '1 / -1' }}>
          <label style={{ display: 'block', fontSize: 12, color: 'var(--text-secondary)', marginBottom: 6, fontWeight: 500 }}>Tags</label>
          <div style={{ display: 'flex', gap: 6, flexWrap: 'wrap' }}>
            {tags.map(tag => (
              <button key={tag} onClick={() => toggleTag(tag)} style={{
                padding: '3px 12px', borderRadius: 'var(--radius-full)', fontSize: 12, cursor: 'pointer',
                background: selectedTags.includes(tag) ? 'var(--accent)' : '#2a2a2a',
                color: selectedTags.includes(tag) ? '#fff' : 'var(--text-secondary)',
                border: 'none', fontWeight: 500,
              }}>
                {tag}
              </button>
            ))}
          </div>
        </div>
      </div>
      <div style={{ marginTop: 14, display: 'flex', gap: 8 }}>
        <Button variant="primary" disabled={!valid} onClick={() => onSave({ endpoint, model, provider, tags: selectedTags, format, enabled: initial?.enabled ?? true })}>Save</Button>
        <Button variant="ghost" onClick={onCancel}>Cancel</Button>
      </div>
    </Card>
  );
}
