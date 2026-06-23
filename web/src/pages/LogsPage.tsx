import { useState, useEffect, useMemo } from 'react';
import type { RequestLog, DailyUsage, CostRate } from '../lib/api';
import * as api from '../lib/api';
import { Badge } from '../components/Badge';

function tagColor(tag: string): string {
  switch (tag) {
    case 'haiku': return '#22C55E';
    case 'sonnet': return '#3B82F6';
    case 'opus': return '#A855F7';
    case 'auto': return '#F59E0B';
    default: return '#555';
  }
}

function relativeTime(timestampMs: number): string {
  const now = Date.now();
  const diff = Math.floor((now - timestampMs) / 1000);
  if (diff < 5) return 'just now';
  if (diff < 60) return `${diff}s ago`;
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
  return new Date(timestampMs).toLocaleString();
}

function formatDate(d: Date): string {
  return d.toISOString().slice(0, 10);
}

export function LogsPage() {
  const [logs, setLogs] = useState<RequestLog[]>([]);
  const [daily, setDaily] = useState<DailyUsage[]>([]);
  const [rates, setRates] = useState<CostRate[]>([]);
  const [error, setError] = useState<string | null>(null);

  const today = new Date();
  const from = formatDate(new Date(today.getFullYear(), today.getMonth(), 1));
  const to = formatDate(today);

  const refresh = async () => {
    try {
      const [logsList, dailyUsage, rateList] = await Promise.all([
        api.getLogs(),
        api.getDailyUsage(from, to, null),
        api.listCostRates(),
      ]);
      setLogs(logsList);
      setDaily(dailyUsage);
      setRates(rateList);
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to load data');
    }
  };

  useEffect(() => {
    refresh();
    // Only poll when tab is visible, and at a slower interval
    let interval: ReturnType<typeof setInterval> | null = null;
    const startPolling = () => {
      if (!interval) interval = setInterval(refresh, 8000);
    };
    const stopPolling = () => {
      if (interval) { clearInterval(interval); interval = null; }
    };
    const handleVisibility = () => {
      if (document.hidden) stopPolling();
      else { refresh(); startPolling(); }
    };
    document.addEventListener('visibilitychange', handleVisibility);
    startPolling();
    return () => {
      stopPolling();
      document.removeEventListener('visibilitychange', handleVisibility);
    };
  }, [from, to]);

  const totals = useMemo(() => {
    return daily.reduce(
      (acc, row) => ({
        requests: acc.requests + row.request_count,
        input: acc.input + row.input_tokens,
        output: acc.output + row.output_tokens,
        cost: acc.cost + row.estimated_cost,
      }),
      { requests: 0, input: 0, output: 0, cost: 0 }
    );
  }, [daily]);

  return (
    <div>
      <h3 style={{ margin: '0 0 16px', fontSize: 16 }}>Dashboard</h3>
      {error && <div style={{ color: '#EF4444', fontSize: 13, marginBottom: 12 }}>{error}</div>}

      {/* Summary Cards */}
      <div style={{ display: 'grid', gridTemplateColumns: 'repeat(4, 1fr)', gap: 12, marginBottom: 20 }}>
        <SummaryCard label="Requests" value={totals.requests.toLocaleString()} />
        <SummaryCard label="Input tokens" value={totals.input.toLocaleString()} />
        <SummaryCard label="Output tokens" value={totals.output.toLocaleString()} />
        <SummaryCard label="Estimated cost" value={`$${totals.cost.toFixed(4)}`} />
      </div>

      {/* Recent Requests — merged with per-request usage */}
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 16 }}>
        <h4 style={{ fontSize: 14, margin: 0 }}>Recent Requests</h4>
        <span style={{ fontSize: 11, color: 'var(--text-muted)' }}>Auto-refreshes every 3s</span>
      </div>

      {logs.length === 0 ? (
        <div style={{
          padding: 48, textAlign: 'center', color: 'var(--text-muted)',
          border: '1px dashed var(--border)', borderRadius: 'var(--radius-md)',
        }}>
          <div style={{ fontSize: 28, marginBottom: 8 }}>📡</div>
          <div>No requests yet</div>
          <div style={{ fontSize: 12, marginTop: 4 }}>Send a message through Claude Code to see logs</div>
        </div>
      ) : (
        <div style={{ border: '1px solid var(--border)', borderRadius: 'var(--radius-md)', overflow: 'hidden' }}>
          <table>
            <thead>
              <tr>
                <th>Time</th>
                <th>Caller</th>
                <th>Tag</th>
                <th>Provider</th>
                <th>Target</th>
                <th style={{ textAlign: 'right' }}>Input</th>
                <th style={{ textAlign: 'right' }}>Output</th>
                <th style={{ textAlign: 'right' }}>Latency</th>
                <th style={{ textAlign: 'right' }}>Cost</th>
              </tr>
            </thead>
            <tbody>
              {[...logs].sort((a, b) => b.timestamp_ms - a.timestamp_ms).map((log, i) => (
                <tr key={i}>
                  <td className="mono" style={{ fontSize: 12, color: 'var(--text-muted)', whiteSpace: 'nowrap' }} title={log.timestamp}>
                    {relativeTime(log.timestamp_ms)}
                  </td>
                  <td style={{ fontSize: 12, color: 'var(--text-secondary)', fontWeight: 500 }}>
                    {log.caller_key_name || 'Unknown'}
                  </td>
                  <td>
                    <Badge color={tagColor(log.tag)}>{log.tag}</Badge>
                  </td>
                  <td style={{ color: 'var(--text-secondary)' }}>{log.provider}</td>
                  <td className="mono" style={{ fontSize: 12, color: 'var(--text-muted)' }}>{log.target_model}</td>
                  <td style={{ fontSize: 12, textAlign: 'right' }}>{log.input_tokens?.toLocaleString() ?? '-'}</td>
                  <td style={{ fontSize: 12, textAlign: 'right' }}>{log.output_tokens?.toLocaleString() ?? '-'}</td>
                  <td style={{ fontSize: 12, textAlign: 'right' }}>{log.latency_ms}ms</td>
                  <td style={{ fontSize: 12, textAlign: 'right' }}>${log.cost.toFixed(4)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {/* Cost rates */}
      <h4 style={{ fontSize: 14, margin: '24px 0 10px' }}>Cost rates</h4>
      <CostRatesTable rates={rates} onChange={refresh} />
    </div>
  );
}

function SummaryCard({ label, value }: { label: string; value: string }) {
  return (
    <div style={{
      background: 'var(--bg-card, #141414)', border: '1px solid var(--border)',
      borderRadius: 8, padding: 14,
    }}>
      <div style={{ fontSize: 12, color: 'var(--text-muted)', marginBottom: 6 }}>{label}</div>
      <div style={{ fontSize: 22, fontWeight: 800, color: 'var(--text-primary)', letterSpacing: '-0.3px' }}>{value}</div>
    </div>
  );
}

function CostRatesTable({ rates, onChange }: { rates: CostRate[]; onChange: () => void }) {
  const [provider, setProvider] = useState('');
  const [model, setModel] = useState('');
  const [inputPrice, setInputPrice] = useState('');
  const [outputPrice, setOutputPrice] = useState('');
  const [error, setError] = useState<string | null>(null);

  const handleAdd = async () => {
    try {
      await api.setCostRate({
        provider: provider.trim(),
        model: model.trim(),
        input_price_per_1k: parseFloat(inputPrice) || 0,
        output_price_per_1k: parseFloat(outputPrice) || 0,
      });
      setProvider('');
      setModel('');
      setInputPrice('');
      setOutputPrice('');
      onChange();
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to set rate');
    }
  };

  const handleDelete = async (id: number) => {
    if (!confirm('Delete this cost rate?')) return;
    try {
      await api.deleteCostRate(id);
      onChange();
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to delete rate');
    }
  };

  return (
    <div>
      {error && <div style={{ color: '#EF4444', fontSize: 13, marginBottom: 10 }}>{error}</div>}
      <div style={{
        display: 'grid', gridTemplateColumns: '1fr 1fr 1fr 1fr auto', gap: 8, marginBottom: 12,
        alignItems: 'end',
      }}>
        <input type="text" placeholder="Provider" value={provider} onChange={(e) => setProvider(e.target.value)} style={inputStyle} />
        <input type="text" placeholder="Model" value={model} onChange={(e) => setModel(e.target.value)} style={inputStyle} />
        <input type="number" step="0.0001" placeholder="Input $/1k" value={inputPrice} onChange={(e) => setInputPrice(e.target.value)} style={inputStyle} />
        <input type="number" step="0.0001" placeholder="Output $/1k" value={outputPrice} onChange={(e) => setOutputPrice(e.target.value)} style={inputStyle} />
        <button
          onClick={handleAdd}
          disabled={!provider.trim() || !model.trim()}
          style={{
            padding: '8px 14px', fontSize: 13, fontWeight: 600,
            background: 'var(--accent)', color: '#fff', border: 'none',
            borderRadius: 6, cursor: 'pointer', opacity: !provider.trim() || !model.trim() ? 0.5 : 1,
          }}
        >Add</button>
      </div>

      <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: 13 }}>
        <thead>
          <tr style={{ borderBottom: '1px solid var(--border)' }}>
            <th style={thStyle}>Provider</th>
            <th style={thStyle}>Model</th>
            <th style={{ ...thStyle, textAlign: 'right' }}>Input $/1k</th>
            <th style={{ ...thStyle, textAlign: 'right' }}>Output $/1k</th>
            <th style={{ ...thStyle, textAlign: 'right' }}>Actions</th>
          </tr>
        </thead>
        <tbody>
          {rates.map((rate) => (
            <tr key={rate.id} style={{ borderBottom: '1px solid var(--border)' }}>
              <td style={{ padding: '8px 0' }}>{rate.provider}</td>
              <td style={{ padding: '8px 0' }}>{rate.model}</td>
              <td style={{ padding: '8px 0', textAlign: 'right' }}>{rate.input_price_per_1k.toFixed(4)}</td>
              <td style={{ padding: '8px 0', textAlign: 'right' }}>{rate.output_price_per_1k.toFixed(4)}</td>
              <td style={{ padding: '8px 0', textAlign: 'right' }}>
                <button
                  onClick={() => handleDelete(rate.id)}
                  style={{
                    background: 'transparent', border: '1px solid var(--border)',
                    borderRadius: 6, padding: '3px 8px', fontSize: 12,
                    color: '#EF4444', cursor: 'pointer',
                  }}
                >Delete</button>
              </td>
            </tr>
          ))}
          {rates.length === 0 && (
            <tr>
              <td colSpan={5} style={{ padding: 16, textAlign: 'center', color: 'var(--text-muted)' }}>
                No cost rates configured.
              </td>
            </tr>
          )}
        </tbody>
      </table>
    </div>
  );
}

const inputStyle: React.CSSProperties = {
  padding: '8px 10px', fontSize: 13,
  border: '1px solid var(--border)', borderRadius: 6,
  background: 'var(--bg-primary)', color: 'var(--text-primary)',
  boxSizing: 'border-box', width: '100%',
};

const thStyle: React.CSSProperties = { textAlign: 'left', padding: '8px 0', color: 'var(--text-muted)', fontWeight: 500 };
