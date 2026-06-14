import { Card } from '../components/Card';
import { Button } from '../components/Button';
import { Badge } from '../components/Badge';

interface LandingPageProps {
  onEnter: () => void;
  setupRequired: boolean;
}

const GITHUB = 'https://github.com/yinnho/AginxBrain';

const sectionStyle: React.CSSProperties = {
  maxWidth: 1000,
  margin: '0 auto',
  padding: '56px 24px',
};

const sectionTitleStyle: React.CSSProperties = {
  fontSize: 24,
  fontWeight: 700,
  color: 'var(--text-primary)',
  marginBottom: 8,
  letterSpacing: '-0.3px',
};

const sectionSubStyle: React.CSSProperties = {
  fontSize: 14,
  color: 'var(--text-secondary)',
  marginBottom: 28,
};

const valueProps = [
  {
    icon: '🧠',
    title: '统一 AI 能力入口',
    body: 'Agent 只对接一个 endpoint，背后多 provider 自动调度。对话、生图、嵌入，一个网关全包。',
  },
  {
    icon: '🔑',
    title: 'Agent 不接触真实 API Key',
    body: '调用方密钥隔离，真实 provider key 留在服务端。发密钥、停密钥、轮换，都在控制台完成。',
  },
  {
    icon: '🔀',
    title: '智能路由 + 自动 failover',
    body: '标签路由（opus / sonnet / haiku / auto）、协议格式转换、故障自动转移、调用全程审计。',
  },
  {
    icon: '📊',
    title: '用量与成本可观测',
    body: '按调用方、provider、标签统计 token 与费用。一眼看清谁在用、用了多少、花了多少钱。',
  },
];

// 协议转换矩阵：客户端协议（行）× provider 格式（列）。
// 直通 = 客户端与 provider 格式一致；转换 = 网关自动转格式。
const clients = ['Anthropic', 'OpenAI', 'Responses'];
const providers = ['OpenAI', 'Anthropic', 'Responses'];
const providerBadges: Record<string, { name: string; color: string }[]> = {
  国产: [
    { name: 'DeepSeek', color: 'var(--accent)' },
    { name: '智谱 GLM', color: '#8B5CF6' },
    { name: 'Kimi (Moonshot)', color: 'var(--success)' },
    { name: '通义千问', color: 'var(--accent)' },
    { name: '百度文心', color: '#8B5CF6' },
    { name: 'MiniMax', color: 'var(--success)' },
  ],
  通用: [
    { name: 'Anthropic', color: 'var(--text-muted)' },
    { name: 'OpenAI', color: 'var(--text-muted)' },
  ],
};

export function LandingPage({ onEnter, setupRequired }: LandingPageProps) {
  return (
    <div style={{ minHeight: '100vh', background: 'var(--bg-primary)' }}>
      {/* Nav */}
      <header style={{
        borderBottom: '1px solid var(--border)',
        position: 'sticky', top: 0,
        background: 'rgba(10,10,10,0.8)', backdropFilter: 'blur(8px)',
        zIndex: 10,
      }}>
        <div style={{ maxWidth: 1000, margin: '0 auto', padding: '14px 24px', display: 'flex', alignItems: 'center', justifyContent: 'space-between' }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
            <div style={{
              width: 30, height: 30, borderRadius: 8,
              background: 'linear-gradient(135deg, var(--accent), #8B5CF6)',
              display: 'flex', alignItems: 'center', justifyContent: 'center',
              fontSize: 15, fontWeight: 700, color: '#fff',
            }}>🧠</div>
            <span style={{ fontSize: 15, fontWeight: 600, color: 'var(--text-primary)' }}>AginxBrain</span>
          </div>
          <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
            <a href={GITHUB} target="_blank" rel="noopener noreferrer"
              style={{ fontSize: 13, color: 'var(--text-secondary)', textDecoration: 'none', padding: '7px 16px', borderRadius: 'var(--radius-sm)', border: '1px solid var(--border)' }}>
              GitHub
            </a>
            <Button variant="primary" onClick={onEnter} style={{ fontSize: 13 }}>
              {setupRequired ? '初始化' : '登录'}
            </Button>
          </div>
        </div>
      </header>

      {/* Hero */}
      <section style={{
        ...sectionStyle,
        paddingTop: 88, paddingBottom: 72,
        textAlign: 'center',
        display: 'flex', flexDirection: 'column', alignItems: 'center',
      }}>
        <Badge color="var(--success)" style={{ marginBottom: 24, background: 'var(--success-dim)', color: 'var(--success)' }}>
          开源 · MIT · 自托管
        </Badge>
        <h1 style={{ fontSize: 44, fontWeight: 800, letterSpacing: '-1px', color: 'var(--text-primary)', marginBottom: 18 }}>
          Agent 的 <span style={{
            background: 'linear-gradient(135deg, var(--accent), #8B5CF6)',
            WebkitBackgroundClip: 'text', WebkitTextFillColor: 'transparent', backgroundClip: 'text',
          }}>AI 大脑</span>
        </h1>
        <h2 style={{ fontSize: 20, fontWeight: 400, color: 'var(--text-secondary)', maxWidth: 620, lineHeight: 1.6, marginBottom: 8 }}>
          Agent 只说需求，AginxBrain 负责剩下的一切。
        </h2>
        <p style={{ fontSize: 14, color: 'var(--text-muted)', marginBottom: 32 }}>
          AI 模型代理 · 协议转换 · 智能路由
        </p>
        <div style={{ display: 'flex', gap: 12, flexWrap: 'wrap', justifyContent: 'center' }}>
          <Button variant="primary" onClick={onEnter} style={{ fontSize: 15, padding: '11px 24px' }}>
            {setupRequired ? '🚀 初始化控制台' : '登录控制台'}
          </Button>
          <a href={GITHUB} target="_blank" rel="noopener noreferrer"
            style={{
              fontSize: 15, padding: '11px 24px', borderRadius: 'var(--radius-sm)',
              border: '1px solid var(--border)', background: 'transparent',
              color: 'var(--text-secondary)', textDecoration: 'none', display: 'inline-flex', alignItems: 'center',
            }}>
            ⭐ GitHub
          </a>
        </div>
        <p style={{ fontSize: 13, color: 'var(--text-muted)', marginTop: 24 }}>
          💡 用国产模型跑 Claude Code / Codex，每年省下几千块
        </p>
      </section>

      {/* Value props */}
      <section style={sectionStyle}>
        <h2 style={sectionTitleStyle}>为什么需要 AginxBrain</h2>
        <p style={sectionSubStyle}>一个网关，解决 Agent 接入 AI 的所有麻烦。</p>
        <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(240px, 1fr))', gap: 16 }}>
          {valueProps.map((v) => (
            <Card key={v.title} hoverable style={{ display: 'flex', flexDirection: 'column', gap: 10 }}>
              <div style={{ fontSize: 26 }}>{v.icon}</div>
              <div style={{ fontSize: 15, fontWeight: 600, color: 'var(--text-primary)' }}>{v.title}</div>
              <div style={{ fontSize: 13, color: 'var(--text-secondary)', lineHeight: 1.6 }}>{v.body}</div>
            </Card>
          ))}
        </div>
      </section>

      {/* Protocol matrix */}
      <section style={sectionStyle}>
        <h2 style={sectionTitleStyle}>9 种协议转换组合</h2>
        <p style={sectionSubStyle}>客户端协议 × provider 格式，全覆盖。流式 SSE、thinking 块、tool_use 块全部正确处理。</p>
        <Card style={{ padding: 0, overflow: 'hidden' }}>
          <table>
            <thead>
              <tr>
                <th>客户端协议</th>
                {providers.map((p) => <th key={p} style={{ textAlign: 'center' }}>Provider · {p}</th>)}
              </tr>
            </thead>
            <tbody>
              {clients.map((c) => (
                <tr key={c}>
                  <td>
                    <Badge color="var(--accent)">{c}</Badge>
                  </td>
                  {providers.map((p) => {
                    const passthrough = c === p;
                    return (
                      <td key={p} style={{ textAlign: 'center' }}>
                        <span style={{
                          display: 'inline-block', padding: '3px 12px', borderRadius: 'var(--radius-full)',
                          fontSize: 12,
                          background: passthrough ? 'var(--success-dim)' : 'rgba(139,92,246,0.14)',
                          color: passthrough ? 'var(--success)' : '#a78bfa',
                        }}>
                          {passthrough ? '直通' : '转换'}
                        </span>
                      </td>
                    );
                  })}
                </tr>
              ))}
            </tbody>
          </table>
        </Card>
      </section>

      {/* Providers */}
      <section style={sectionStyle}>
        <h2 style={sectionTitleStyle}>支持的 Provider</h2>
        <p style={sectionSubStyle}>主流国产模型 + 国际模型，通过标签一键路由。</p>
        <div style={{ display: 'flex', flexWrap: 'wrap', gap: 10, marginBottom: 8 }}>
          {providerBadges.国产.map((p) => (
            <Badge key={p.name} color={p.color} style={{ padding: '5px 14px', fontSize: 13 }}>{p.name}</Badge>
          ))}
          {providerBadges.通用.map((p) => (
            <Badge key={p.name} color={p.color} style={{ padding: '5px 14px', fontSize: 13 }}>{p.name}</Badge>
          ))}
        </div>
        <p style={{ fontSize: 12, color: 'var(--text-muted)', marginTop: 12 }}>
          标签质量分级：opus（最强）· sonnet（均衡）· haiku（快速）· auto（自动）
        </p>
      </section>

      {/* Open source / self-host */}
      <section style={sectionStyle}>
        <Card style={{ padding: 32, display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 24, flexWrap: 'wrap' }}>
          <div>
            <div style={{ fontSize: 18, fontWeight: 700, color: 'var(--text-primary)', marginBottom: 8 }}>
              开源 · 自托管 · 零依赖
            </div>
            <div style={{ fontSize: 13, color: 'var(--text-secondary)', maxWidth: 520, lineHeight: 1.6 }}>
              MIT 协议，单个 Rust 二进制，内嵌 SQLite。一行命令启动，数据完全在你自己手里。
              适合个人开发者本地跑，也适合团队 / 公司内部部署统一管理。
            </div>
          </div>
          <a href={GITHUB} target="_blank" rel="noopener noreferrer"
            style={{
              fontSize: 14, padding: '10px 20px', borderRadius: 'var(--radius-sm)',
              border: '1px solid var(--border)', background: 'transparent',
              color: 'var(--text-primary)', textDecoration: 'none', whiteSpace: 'nowrap',
            }}>
            查看源码 →
          </a>
        </Card>
      </section>

      {/* Final CTA */}
      <section style={{ ...sectionStyle, textAlign: 'center', paddingTop: 32, paddingBottom: 80 }}>
        <h2 style={{ fontSize: 28, fontWeight: 700, color: 'var(--text-primary)', marginBottom: 12 }}>
          准备好了吗？
        </h2>
        <p style={{ fontSize: 14, color: 'var(--text-secondary)', marginBottom: 28 }}>
          {setupRequired ? '创建管理员账户，一分钟接入你的 Agent。' : '登录控制台，管理你的 provider 与密钥。'}
        </p>
        <Button variant="primary" onClick={onEnter} style={{ fontSize: 15, padding: '12px 28px' }}>
          {setupRequired ? '🚀 初始化控制台' : '登录控制台'}
        </Button>
      </section>

      {/* Footer */}
      <footer style={{ borderTop: '1px solid var(--border)', padding: '28px 24px', textAlign: 'center' }}>
        <div style={{ fontSize: 13, color: 'var(--text-muted)', marginBottom: 6 }}>
          <a href={GITHUB} target="_blank" rel="noopener noreferrer" style={{ color: 'var(--text-secondary)', textDecoration: 'none' }}>
            MIT · AginxBrain
          </a>
        </div>
        <div style={{ fontSize: 12, color: 'var(--text-muted)' }}>
          Aginx 生态的 AI 能力层 · Agent 只说需求，AginxBrain 负责剩下的一切
        </div>
      </footer>
    </div>
  );
}
