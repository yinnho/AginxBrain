import { useEffect, useMemo, useState } from 'react';
import * as api from '../lib/api';

export function UsagePage() {
  const [daily, setDaily] = useState<api.DailyUsage[]>([]);
  const [, setSummary] = useState<api.UsageSummary[]>([]);
  const [keys, setKeys] = useState<api.CallerKey[]>([]);
  const [rates, setRates] = useState<api.CostRate[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [selectedKeyId, setSelectedKeyId] = useState<string>('all');

  const today = new Date();
  const [from, setFrom] = useState(formatDate(new Date(today.getFullYear(), today.getMonth(), 1)));
  const [to, setTo] = useState(formatDate(today));

  const refresh = async () => {
    try {
      setLoading(true);
      const [keysList, dailyUsage, usageSummary, rateList] = await Promise.all([
        api.listKeys(),
        api.getDailyUsage(from, to, selectedKeyId === 'all' ? null : Number(selectedKeyId)),
        api.getUsageSummary(),
        api.listCostRates(),
      ]);
      setKeys(keysList);
      setDaily(dailyUsage);
      setSummary(usageSummary);
      setRates(rateList);
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to load usage');
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    refresh();
  }, [from, to, selectedKeyId]);

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
      <h3 style={{ margin: '0 0 16px', fontSize: 16 }}>Usage &amp; Cost</h3>
      {error && <div style={{ color: '#EF4444', fontSize: 13, marginBottom: 12 }}>{error}</div>}

      <div style={{
        display: 'grid', gridTemplateColumns: 'repeat(4, 1fr)', gap: 12, marginBottom: 20,
      }}>
        <SummaryCard label="Requests" value={totals.requests.toLocaleString()} />
        <SummaryCard label="Input tokens" value={totals.input.toLocaleString()} />
        <SummaryCard label="Output tokens" value={totals.output.toLocaleString()} />
        <SummaryCard label="Estimated cost" value={`$${totals.cost.toFixed(4)}`} />
      </div>

      <div style={{ display: 'flex', gap: 12, marginBottom: 16, alignItems: 'end' }}>
        <div>
          <label style={{ fontSize: 12, color: 'var(--text-muted)', display: 'block', marginBottom: 4 }}>From</label>
          <input
            type="date"
            value={from}
            onChange={(e) => setFrom(e.target.value)}
            style={{
              padding: '6px 10px', fontSize: 13, border: '1px solid var(--border)',
              borderRadius: 6, background: 'var(--bg-primary)', color: 'var(--text-primary)',
            }}
          />
        </div>
        <div>
          <label style={{ fontSize: 12, color: 'var(--text-muted)', display: 'block', marginBottom: 4 }}>To</label>
          <input
            type="date"
            value={to}
            onChange={(e) => setTo(e.target.value)}
            style={{
              padding: '6px 10px', fontSize: 13, border: '1px solid var(--border)',
              borderRadius: 6, background: 'var(--bg-primary)', color: 'var(--text-primary)',
            }}
          />
        </div>
        <div>
          <label style={{ fontSize: 12, color: 'var(--text-muted)', display: 'block', marginBottom: 4 }}>API Key</label>
          <select
            value={selectedKeyId}
            onChange={(e) => setSelectedKeyId(e.target.value)}
            style={{
              padding: '6px 10px', fontSize: 13, border: '1px solid var(--border)',
              borderRadius: 6, background: 'var(--bg-primary)', color: 'var(--text-primary)',
            }}
          >
            <option value="all">All keys</option>
            {keys.map((k) => (
              <option key={k.id} value={k.id}>{k.name}</option>
            ))}
          </select>
        </div>
      </div>

      <h4 style={{ fontSize: 14, margin: '20px 0 10px' }}>Daily breakdown</h4>
      {loading ? (
        <div style={{ color: 'var(--text-muted)', fontSize: 13 }}>Loading...</div>
      ) : (
        <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: 13 }}>
          <thead>
            <tr style={{ borderBottom: '1px solid var(--border)' }}>
              <th style={{ textAlign: 'left', padding: '8px 0', color: 'var(--text-muted)', fontWeight: 500 }}>Day</th>
              <th style={{ textAlign: 'right', padding: '8px 0', color: 'var(--text-muted)', fontWeight: 500 }}>Requests</th>
              <th style={{ textAlign: 'right', padding: '8px 0', color: 'var(--text-muted)', fontWeight: 500 }}>Input tokens</th>
              <th style={{ textAlign: 'right', padding: '8px 0', color: 'var(--text-muted)', fontWeight: 500 }}>Output tokens</th>
              <th style={{ textAlign: 'right', padding: '8px 0', color: 'var(--text-muted)', fontWeight: 500 }}>Cost</th>
            </tr>
          </thead>
          <tbody>
            {daily.map((row) => (
              <tr key={`${row.day}-${row.caller_key_id ?? 'all'}`} style={{ borderBottom: '1px solid var(--border)' }}>
                <td style={{ padding: '10px 0' }}>{row.day}</td>
                <td style={{ padding: '10px 0', textAlign: 'right' }}>{row.request_count.toLocaleString()}</td>
                <td style={{ padding: '10px 0', textAlign: 'right' }}>{row.input_tokens.toLocaleString()}</td>
                <td style={{ padding: '10px 0', textAlign: 'right' }}>{row.output_tokens.toLocaleString()}</td>
                <td style={{ padding: '10px 0', textAlign: 'right' }}>${row.estimated_cost.toFixed(4)}</td>
              </tr>
            ))}
            {daily.length === 0 && (
              <tr>
                <td colSpan={5} style={{ padding: 20, textAlign: 'center', color: 'var(--text-muted)' }}>
                  No usage in selected range.
                </td>
              </tr>
            )}
          </tbody>
        </table>
      )}

      <h4 style={{ fontSize: 14, margin: '24px 0 10px' }}>Cost rates</h4>
      <CostRatesTable rates={rates} onChange={refresh} />
    </div>
  );
}

function SummaryCard({ label, value }: { label: string; value: string }) {
  return (
    <div style={{
      background: 'var(--bg-secondary, #f8fafc)', border: '1px solid var(--border)',
      borderRadius: 8, padding: 14,
    }}>
      <div style={{ fontSize: 12, color: 'var(--text-muted)', marginBottom: 6 }}>{label}</div>
      <div style={{ fontSize: 20, fontWeight: 700 }}>{value}</div>
    </div>
  );
}

function CostRatesTable({ rates, onChange }: { rates: api.CostRate[]; onChange: () => void }) {
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

function formatDate(d: Date): string {
  return d.toISOString().slice(0, 10);
}
