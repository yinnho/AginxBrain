import { useState, useEffect } from 'react';
import type { RequestLog } from '../lib/api';
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

function relativeTime(ts: string): string {
  const now = Date.now();
  const then = new Date(ts).getTime();
  const diff = Math.floor((now - then) / 1000);
  if (diff < 5) return 'just now';
  if (diff < 60) return `${diff}s ago`;
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
  return ts.replace('T', ' ').replace('Z', '').slice(0, 19);
}

export function LogsPage() {
  const [logs, setLogs] = useState<RequestLog[]>([]);

  useEffect(() => {
    const load = () => api.getLogs().then(setLogs);
    load();
    const interval = setInterval(load, 3000);
    return () => clearInterval(interval);
  }, []);

  return (
    <div>
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 16 }}>
        <h2 style={{ margin: 0, fontSize: 16, fontWeight: 600 }}>Request Log</h2>
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
                <th>Model</th>
                <th>Tag</th>
                <th>Provider</th>
                <th>Target</th>
              </tr>
            </thead>
            <tbody>
              {[...logs].reverse().map((log, i) => (
                <tr key={i}>
                  <td className="mono" style={{ fontSize: 12, color: 'var(--text-muted)', whiteSpace: 'nowrap' }} title={log.timestamp}>
                    {relativeTime(log.timestamp)}
                  </td>
                  <td className="mono" style={{ color: 'var(--text-primary)', fontWeight: 500 }}>
                    {log.request_model}
                  </td>
                  <td>
                    <Badge color={tagColor(log.tag)}>{log.tag}</Badge>
                  </td>
                  <td style={{ color: 'var(--text-secondary)' }}>{log.provider}</td>
                  <td className="mono" style={{ fontSize: 12, color: 'var(--text-muted)' }}>{log.target_model}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
