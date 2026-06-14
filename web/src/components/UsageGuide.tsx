import { useState } from 'react';

interface UsageGuideProps {
  token: string; // real token, or a placeholder like "<your-api-key>"
  dense?: boolean;
}

const endpoints = [
  { method: 'POST', path: '/v1/chat/completions', desc: 'OpenAI Chat' },
  { method: 'POST', path: '/v1/messages', desc: 'Anthropic Messages' },
  { method: 'POST', path: '/v1/responses', desc: 'OpenAI Responses (Codex)' },
  { method: 'GET', path: '/v1/models', desc: 'Model list' },
];

function CodeBlock({ code }: { code: string }) {
  const [copied, setCopied] = useState(false);
  const copy = () => {
    navigator.clipboard?.writeText(code).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    });
  };
  return (
    <div style={{ position: 'relative', marginBottom: 12 }}>
      <button
        onClick={copy}
        style={{
          position: 'absolute', top: 8, right: 8,
          background: 'var(--bg-hover)', border: '1px solid var(--border)',
          borderRadius: 'var(--radius-sm)', padding: '3px 8px', fontSize: 11,
          color: 'var(--text-secondary)', cursor: 'pointer', zIndex: 1,
        }}
      >
        {copied ? '✓ Copied' : 'Copy'}
      </button>
      <pre className="mono" style={{
        margin: 0, padding: 14, paddingRight: 64,
        background: 'var(--bg-primary)', border: '1px solid var(--border)',
        borderRadius: 'var(--radius-sm)', fontSize: 12, lineHeight: 1.6,
        overflowX: 'auto', whiteSpace: 'pre', color: 'var(--text-secondary)',
      }}>
        {code}
      </pre>
    </div>
  );
}

function Subhead({ children }: { children: React.ReactNode }) {
  return (
    <div style={{ fontSize: 12, fontWeight: 600, color: 'var(--text-secondary)', margin: '16px 0 8px', textTransform: 'uppercase', letterSpacing: 0.5 }}>
      {children}
    </div>
  );
}

export function UsageGuide({ token, dense }: UsageGuideProps) {
  const base = typeof window !== 'undefined' ? window.location.origin : 'https://brain.aginx.net';

  const curlOpenAi = `curl ${base}/v1/chat/completions \\
  -H "Authorization: Bearer ${token}" \\
  -H "Content-Type: application/json" \\
  -d '{
    "model": "sonnet",
    "messages": [{"role": "user", "content": "你好"}]
  }'`;

  const curlAnthropic = `curl ${base}/v1/messages \\
  -H "x-api-key: ${token}" \\
  -H "Content-Type: application/json" \\
  -d '{
    "model": "sonnet",
    "max_tokens": 1024,
    "messages": [{"role": "user", "content": "你好"}]
  }'`;

  const claudeCode = `# Claude Code
export ANTHROPIC_BASE_URL=${base}
export ANTHROPIC_API_KEY=${token}
claude`;

  const codex = `# Codex CLI (~/.codex/config.toml)
[model_providers.aginxbrain]
name = "AginxBrain"
base_url = "${base}/v1"
env_key = "AGINXBRAIN_API_KEY"
wire_api = "chat"

# then: export AGINXBRAIN_API_KEY=${token}`;

  const sdk = `# OpenAI SDK
client = OpenAI(base_url="${base}/v1", api_key="${token}")

# Anthropic SDK
client = Anthropic(base_url="${base}", api_key="${token}")`;

  return (
    <div>
      <div style={{ display: 'flex', gap: 24, flexWrap: 'wrap', marginBottom: 4 }}>
        <div>
          <div style={{ fontSize: 11, color: 'var(--text-muted)', marginBottom: 2 }}>Base URL</div>
          <div className="mono" style={{ fontSize: 13, color: 'var(--text-primary)' }}>{base}</div>
        </div>
        <div>
          <div style={{ fontSize: 11, color: 'var(--text-muted)', marginBottom: 2 }}>Auth</div>
          <div className="mono" style={{ fontSize: 13, color: 'var(--text-primary)' }}>Authorization: Bearer {token.length > 24 ? token.slice(0, 12) + '…' : token}</div>
          <div style={{ fontSize: 11, color: 'var(--text-muted)' }}>Anthropic 客户端也可用 x-api-key</div>
        </div>
      </div>

      {!dense && (
        <>
          <Subhead>Endpoints</Subhead>
          <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(240px, 1fr))', gap: 6 }}>
            {endpoints.map((e) => (
              <div key={e.path} style={{ display: 'flex', alignItems: 'center', gap: 8, fontSize: 12 }}>
                <span style={{ display: 'inline-block', padding: '1px 6px', borderRadius: 4, background: 'var(--bg-hover)', color: 'var(--text-muted)', fontSize: 10, fontWeight: 600 }}>{e.method}</span>
                <span className="mono" style={{ color: 'var(--text-secondary)' }}>{e.path}</span>
                <span style={{ color: 'var(--text-muted)' }}>{e.desc}</span>
              </div>
            ))}
          </div>
          <div style={{ fontSize: 11, color: 'var(--text-muted)', marginTop: 8 }}>
            model 用标签名：<span className="mono">opus / sonnet / haiku / auto</span>（当前默认 <span className="mono">sonnet</span>）
          </div>
        </>
      )}

      <Subhead>curl — OpenAI 格式</Subhead>
      <CodeBlock code={curlOpenAi} />

      <Subhead>curl — Anthropic 格式</Subhead>
      <CodeBlock code={curlAnthropic} />

      <Subhead>Claude Code / Codex / SDK</Subhead>
      <CodeBlock code={claudeCode} />
      <CodeBlock code={codex} />
      <CodeBlock code={sdk} />
    </div>
  );
}
