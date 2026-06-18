import { useState, useEffect, useCallback } from 'react'

// ─── Tauri command bridge ──────────────────────────────────────────────────
// These invoke Rust commands that write/remove ~/.claude/settings.json and
// ~/.codex/config.toml to point at the remote AginxBrain server.
const isTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauri) throw new Error('not in tauri')
  const { invoke: tauriInvoke } = await import('@tauri-apps/api/core')
  return tauriInvoke<T>(cmd, args)
}

interface TakeoverState {
  claude: boolean
  codex: boolean
}

// ─── Local persistence ─────────────────────────────────────────────────────
const SERVER_KEY = 'aginxbrain_server'
const APIKEY_KEY = 'aginxbrain_api_key'

const DEFAULT_SERVER = 'https://brain.aginx.net'

// ─── Helpers ───────────────────────────────────────────────────────────────
function tagColor(tag: string): string {
  switch (tag) {
    case 'haiku': return '#22C55E'
    case 'sonnet': return '#3B82F6'
    case 'opus': return '#A855F7'
    case 'auto': return '#F59E0B'
    default: return '#555'
  }
}

function relativeTime(ms: number): string {
  const diff = Math.floor((Date.now() - ms) / 1000)
  if (diff < 5) return '刚刚'
  if (diff < 60) return `${diff}秒前`
  if (diff < 3600) return `${Math.floor(diff / 60)}分钟前`
  if (diff < 86400) return `${Math.floor(diff / 3600)}小时前`
  return new Date(ms).toLocaleString('zh-CN', { timeZone: 'Asia/Shanghai' })
}

interface LogEntry {
  tag: string
  provider: string
  input_tokens?: number | null
  output_tokens?: number | null
  latency_ms: number
  cost: number
  timestamp_ms: number
}

// ─── App ───────────────────────────────────────────────────────────────────
function App() {
  const [server, setServer] = useState(() => localStorage.getItem(SERVER_KEY) || DEFAULT_SERVER)
  const [apiKey, setApiKey] = useState(() => localStorage.getItem(APIKEY_KEY) || '')
  const [tempServer, setTempServer] = useState(server)
  const [tempKey, setTempKey] = useState('')
  const [connected, setConnected] = useState(false)
  const [connError, setConnError] = useState<string | null>(null)
  const [logs, setLogs] = useState<LogEntry[]>([])
  const [takeover, setTakeover] = useState<TakeoverState>({ claude: false, codex: false })
  const [busy, setBusy] = useState<string | null>(null)

  const authHeaders = useCallback(() => ({ Authorization: `Bearer ${apiKey}` }), [apiKey])

  // ─── Refresh logs + takeover status ───
  const refresh = useCallback(async () => {
    if (!apiKey || !server) return
    try {
      const [logsRes] = await Promise.all([
        fetch(`${server}/api/logs`, { headers: authHeaders() }),
      ])
      if (!logsRes.ok) {
        setConnected(false)
        setConnError(logsRes.status === 401 ? 'API Key 无效' : `服务错误 (${logsRes.status})`)
        return
      }
      setConnected(true)
      setConnError(null)
      setLogs(await logsRes.json())
    } catch {
      setConnected(false)
      setConnError('无法连接到服务器')
    }
  }, [apiKey, server, authHeaders])

  // ─── Check takeover status (local config files) via Tauri ───
  const refreshTakeover = useCallback(async () => {
    if (!isTauri) return
    try {
      const state = await invoke<TakeoverState>('get_takeover_state')
      setTakeover(state)
    } catch {
      /* ignore */
    }
  }, [])

  useEffect(() => {
    if (apiKey && server) {
      refresh()
      refreshTakeover()
      const i = setInterval(() => { refresh(); refreshTakeover() }, 5000)
      return () => clearInterval(i)
    }
  }, [apiKey, server, refresh, refreshTakeover])

  // ─── Connect (save server + key) ───
  const handleConnect = () => {
    const s = tempServer.trim().replace(/\/+$/, '') || DEFAULT_SERVER
    const k = tempKey.trim()
    if (!k) return
    setServer(s)
    setApiKey(k)
    localStorage.setItem(SERVER_KEY, s)
    localStorage.setItem(APIKEY_KEY, k)
    setTempKey('')
  }

  // ─── Disconnect ───
  const handleDisconnect = () => {
    setApiKey('')
    localStorage.removeItem(APIKEY_KEY)
    setLogs([])
    setConnected(false)
  }

  // ─── Toggle Claude Code takeover ───
  const toggleClaude = async () => {
    if (!isTauri) { setConnError('需在桌面应用内操作'); return }
    setBusy('claude')
    try {
      const active = await invoke<boolean>(
        takeover.claude ? 'restore_claude' : 'takeover_claude',
        { server, apiKey }
      )
      setTakeover(s => ({ ...s, claude: active }))
    } catch (e) {
      setConnError(`Claude Code 接管失败: ${e}`)
    } finally {
      setBusy(null)
    }
  }

  const toggleCodex = async () => {
    if (!isTauri) { setConnError('需在桌面应用内操作'); return }
    setBusy('codex')
    try {
      const active = await invoke<boolean>(
        takeover.codex ? 'restore_codex' : 'takeover_codex',
        { server, apiKey }
      )
      setTakeover(s => ({ ...s, codex: active }))
    } catch (e) {
      setConnError(`Codex 接管失败: ${e}`)
    } finally {
      setBusy(null)
    }
  }

  // ─── Setup screen ───
  if (!apiKey) {
    return (
      <div style={centerWrap}>
        <div style={{ width: 380, padding: 32 }}>
          <div style={{ textAlign: 'center', marginBottom: 28 }}>
            <Logo />
            <h1 style={{ fontSize: 22, fontWeight: 700, marginBottom: 6 }}>AginxBrain</h1>
            <p style={{ fontSize: 13, color: 'var(--text-secondary)' }}>输入 API Key 连接</p>
          </div>

          <label style={fieldLabel}>服务器地址</label>
          <input
            type="text"
            value={tempServer}
            onChange={e => setTempServer(e.target.value)}
            placeholder={DEFAULT_SERVER}
            style={{ ...inputStyle, marginBottom: 14 }}
          />

          <label style={fieldLabel}>API Key</label>
          <input
            type="password"
            value={tempKey}
            onChange={e => setTempKey(e.target.value)}
            placeholder="sk-..."
            onKeyDown={e => { if (e.key === 'Enter') handleConnect() }}
            style={{ ...inputStyle, marginBottom: 18 }}
          />

          <button
            onClick={handleConnect}
            disabled={!tempKey.trim()}
            style={primaryBtn(!tempKey.trim())}
          >
            连接
          </button>

          <p style={{ fontSize: 12, color: 'var(--text-muted)', textAlign: 'center', marginTop: 16 }}>
            在 <a href="https://brain.aginx.net" target="_blank" rel="noopener noreferrer" style={{ color: 'var(--accent)' }}>brain.aginx.net</a> 获取 API Key
          </p>
        </div>
      </div>
    )
  }

  // ─── Main screen ───
  const today = logs.filter(l => Date.now() - l.timestamp_ms < 86400000)
  const totalIn = today.reduce((s, l) => s + (l.input_tokens || 0), 0)
  const totalOut = today.reduce((s, l) => s + (l.output_tokens || 0), 0)
  const totalCost = today.reduce((s, l) => s + (l.cost || 0), 0)

  return (
    <div style={{ maxWidth: 720, margin: '0 auto', padding: '20px 24px', minHeight: '100vh' }}>
      {/* Header */}
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 24 }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
          <Logo small />
          <div>
            <div style={{ fontSize: 16, fontWeight: 700 }}>AginxBrain</div>
            <div style={{ fontSize: 11, color: connected ? 'var(--success)' : 'var(--danger)' }}>
              {connected ? '已连接' : '未连接'} · {server.replace(/^https?:\/\//, '')}
            </div>
          </div>
        </div>
        <button onClick={handleDisconnect} style={ghostBtn}>切换 Key</button>
      </div>

      {connError && (
        <div style={errorBanner}>{connError}</div>
      )}

      {/* Toggle switches */}
      <div style={{ display: 'flex', gap: 12, marginBottom: 24 }}>
        <ToggleCard
          label="Claude Code"
          desc={takeover.claude ? '已接管' : '未接管'}
          active={takeover.claude}
          loading={busy === 'claude'}
          onToggle={toggleClaude}
        />
        <ToggleCard
          label="Codex"
          desc={takeover.codex ? '已接管' : '未接管'}
          active={takeover.codex}
          loading={busy === 'codex'}
          onToggle={toggleCodex}
        />
      </div>

      {/* Summary */}
      <div style={{ display: 'grid', gridTemplateColumns: 'repeat(4, 1fr)', gap: 10, marginBottom: 24 }}>
        <MiniCard label="今日请求" value={String(today.length)} />
        <MiniCard label="输入 tokens" value={totalIn.toLocaleString()} />
        <MiniCard label="输出 tokens" value={totalOut.toLocaleString()} />
        <MiniCard label="费用" value={`$${totalCost.toFixed(4)}`} />
      </div>

      {/* Logs */}
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 12 }}>
        <h3 style={{ fontSize: 14, fontWeight: 600 }}>最近请求</h3>
        <span style={{ fontSize: 11, color: 'var(--text-muted)' }}>自动刷新</span>
      </div>

      {logs.length === 0 ? (
        <div style={emptyState}>
          <div style={{ fontSize: 24, marginBottom: 8 }}>📡</div>
          <div>暂无请求</div>
          <div style={{ fontSize: 12, marginTop: 4 }}>通过 Claude Code 或 Codex 发送消息后将在此显示</div>
        </div>
      ) : (
        <div style={{ border: '1px solid var(--border)', borderRadius: 8, overflow: 'hidden' }}>
          <table>
            <thead>
              <tr>
                <th>时间</th>
                <th>标签</th>
                <th>Provider</th>
                <th style={{ textAlign: 'right' }}>输入</th>
                <th style={{ textAlign: 'right' }}>输出</th>
                <th style={{ textAlign: 'right' }}>延迟</th>
                <th style={{ textAlign: 'right' }}>费用</th>
              </tr>
            </thead>
            <tbody>
              {[...logs].sort((a, b) => b.timestamp_ms - a.timestamp_ms).slice(0, 50).map((l, i) => (
                <tr key={i}>
                  <td className="mono" style={{ fontSize: 12, color: 'var(--text-muted)', whiteSpace: 'nowrap' }}>{relativeTime(l.timestamp_ms)}</td>
                  <td><span style={{ ...tagBadge, background: `${tagColor(l.tag)}22`, color: tagColor(l.tag) }}>{l.tag}</span></td>
                  <td style={{ fontSize: 12, color: 'var(--text-secondary)' }}>{l.provider}</td>
                  <td style={{ fontSize: 12, textAlign: 'right' }}>{l.input_tokens?.toLocaleString() ?? '-'}</td>
                  <td style={{ fontSize: 12, textAlign: 'right' }}>{l.output_tokens?.toLocaleString() ?? '-'}</td>
                  <td style={{ fontSize: 12, textAlign: 'right' }}>{l.latency_ms}ms</td>
                  <td style={{ fontSize: 12, textAlign: 'right' }}>${l.cost.toFixed(4)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  )
}

// ─── Components ─────────────────────────────────────────────────────────────
function Logo({ small }: { small?: boolean }) {
  const s = small ? 32 : 52
  return (
    <div style={{
      width: s, height: s, borderRadius: small ? 8 : 14,
      background: 'linear-gradient(135deg, var(--accent), #8B5CF6)',
      display: 'flex', alignItems: 'center', justifyContent: 'center',
      margin: small ? 0 : '0 auto 14px', fontSize: small ? 16 : 24,
    }}>🧠</div>
  )
}

function ToggleCard({ label, desc, active, loading, onToggle }: {
  label: string; desc: string; active: boolean; loading?: boolean; onToggle: () => void
}) {
  return (
    <div
      onClick={loading ? undefined : onToggle}
      style={{
        flex: 1, padding: 16,
        background: active ? 'rgba(34,197,94,0.08)' : 'var(--bg-card)',
        border: `1px solid ${active ? 'rgba(34,197,94,0.3)' : 'var(--border)'}`,
        borderRadius: 10, cursor: loading ? 'wait' : 'pointer',
        display: 'flex', alignItems: 'center', justifyContent: 'space-between',
        opacity: loading ? 0.6 : 1,
        transition: 'all 150ms ease',
      }}
    >
      <div>
        <div style={{ fontSize: 14, fontWeight: 600 }}>{label}</div>
        <div style={{ fontSize: 12, color: active ? 'var(--success)' : 'var(--text-muted)', marginTop: 2 }}>
          {loading ? '处理中…' : desc}
        </div>
      </div>
      <div style={{
        width: 44, height: 24, borderRadius: 12,
        background: active ? 'var(--success)' : 'var(--border)',
        position: 'relative', transition: 'background 150ms ease',
      }}>
        <div style={{
          width: 18, height: 18, borderRadius: 9, background: '#fff',
          position: 'absolute', top: 3, left: active ? 23 : 3,
          transition: 'left 150ms ease',
        }} />
      </div>
    </div>
  )
}

function MiniCard({ label, value }: { label: string; value: string }) {
  return (
    <div style={{ background: 'var(--bg-card)', border: '1px solid var(--border)', borderRadius: 8, padding: 12 }}>
      <div style={{ fontSize: 11, color: 'var(--text-muted)', marginBottom: 4 }}>{label}</div>
      <div style={{ fontSize: 18, fontWeight: 700 }}>{value}</div>
    </div>
  )
}

// ─── Styles ─────────────────────────────────────────────────────────────────
const centerWrap: React.CSSProperties = {
  minHeight: '100vh', display: 'flex', alignItems: 'center', justifyContent: 'center',
}
const inputStyle: React.CSSProperties = {
  width: '100%', padding: '12px 14px', fontSize: 14,
  border: '1px solid var(--border)', borderRadius: 8,
  background: 'var(--bg-input)', color: 'var(--text-primary)', outline: 'none',
}
const fieldLabel: React.CSSProperties = {
  display: 'block', fontSize: 12, color: 'var(--text-muted)', marginBottom: 6,
}
const primaryBtn = (disabled: boolean): React.CSSProperties => ({
  width: '100%', padding: '11px 16px', fontSize: 14, fontWeight: 600,
  background: 'var(--accent)', color: '#fff', border: 'none', borderRadius: 8,
  cursor: disabled ? 'default' : 'pointer', opacity: disabled ? 0.5 : 1,
})
const ghostBtn: React.CSSProperties = {
  background: 'transparent', color: 'var(--text-secondary)',
  border: '1px solid var(--border)', borderRadius: 6,
  padding: '4px 10px', fontSize: 12, cursor: 'pointer',
}
const errorBanner: React.CSSProperties = {
  background: 'rgba(239,68,68,0.1)', color: 'var(--danger)',
  border: '1px solid rgba(239,68,68,0.3)',
  borderRadius: 8, padding: '10px 14px', marginBottom: 16, fontSize: 13,
}
const emptyState: React.CSSProperties = {
  padding: 40, textAlign: 'center', color: 'var(--text-muted)',
  border: '1px dashed var(--border)', borderRadius: 8,
}
const tagBadge: React.CSSProperties = {
  display: 'inline-block', padding: '2px 8px', borderRadius: 10,
  fontSize: 11, fontWeight: 600,
}

export default App
