import { useState, useEffect } from 'react';
import type { ProviderHealth } from '../lib/api';
import * as api from '../lib/api';

function statusColor(successRate: number): string {
  if (successRate >= 95) return '#22C55E';
  if (successRate >= 80) return '#F59E0B';
  return '#EF4444';
}

function latencyColor(ms: number): string {
  if (ms < 5000) return '#22C55E';
  if (ms < 15000) return '#F59E0B';
  return '#EF4444';
}

export function DashboardPage() {
  const [data, setData] = useState<ProviderHealth[]>([]);
  const [error, setError] = useState<string | null>(null);

  const refresh = async () => {
    try {
      const health = await api.getProviderHealth();
      setData(health);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to load');
    }
  };

  useEffect(() => {
    refresh();
    let interval: ReturnType<typeof setInterval> | null = null;
    const start = () => { if (!interval) interval = setInterval(refresh, 8000); };
    const stop = () => { if (interval) { clearInterval(interval); interval = null; } };
    const onVis = () => {
      if (document.hidden) stop();
      else { refresh(); start(); }
    };
    document.addEventListener('visibilitychange', onVis);
    start();
    return () => { stop(); document.removeEventListener('visibilitychange', onVis); };
  }, []);

  return (
    <div>
      <h2 style={{ margin: '0 0 16px', fontSize: 16, fontWeight: 600 }}>Provider Health</h2>

      {error && <div style={{ color: '#EF4444', fontSize: 13, marginBottom: 12 }}>{error}</div>}

      {data.length === 0 && !error && (
        <div style={{ padding: 48, textAlign: 'center', color: 'var(--text-muted)',
          border: '1px dashed var(--border)', borderRadius: 'var(--radius-md)' }}>
          No usage data yet. Send some requests through the proxy.
        </div>
      )}

      <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(300px, 1fr))', gap: 12 }}>
        {data.map(p => (
          <div key={p.provider} style={{
            background: 'var(--bg-card)', borderRadius: 'var(--radius-md)', padding: 16,
            border: '1px solid var(--border)',
          }}>
            <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 12 }}>
              <span style={{ fontWeight: 600, fontSize: 14 }}>{p.provider}</span>
              <span style={{
                display: 'inline-block', width: 10, height: 10, borderRadius: '50%',
                background: statusColor(p.success_rate),
              }} title={`${p.success_rate.toFixed(1)}% success`} />
            </div>

            <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: '4px 16px', fontSize: 13 }}>
              <span style={{ color: 'var(--text-muted)' }}>Success rate</span>
              <span style={{ textAlign: 'right', fontWeight: 500, color: statusColor(p.success_rate) }}>
                {p.success_rate.toFixed(1)}%
              </span>

              <span style={{ color: 'var(--text-muted)' }}>Total requests</span>
              <span style={{ textAlign: 'right' }}>{p.total_requests}</span>

              <span style={{ color: 'var(--text-muted)' }}>Avg latency</span>
              <span style={{ textAlign: 'right', fontWeight: 500, color: latencyColor(p.avg_latency_ms) }}>
                {p.avg_latency_ms < 1000 ? `${Math.round(p.avg_latency_ms)}ms` : `${(p.avg_latency_ms / 1000).toFixed(1)}s`}
              </span>

              <span style={{ color: 'var(--text-muted)' }}>Input tokens</span>
              <span style={{ textAlign: 'right' }}>{p.total_input_tokens.toLocaleString()}</span>

              <span style={{ color: 'var(--text-muted)' }}>Output tokens</span>
              <span style={{ textAlign: 'right' }}>{p.total_output_tokens.toLocaleString()}</span>

              <span style={{ color: 'var(--text-muted)' }}>Failures</span>
              <span style={{ textAlign: 'right', color: p.failure_count > 0 ? '#EF4444' : 'var(--text-muted)' }}>
                {p.failure_count}
              </span>

              <span style={{ color: 'var(--text-muted)' }}>Success</span>
              <span style={{ textAlign: 'right', color: '#22C55E' }}>{p.success_count}</span>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
