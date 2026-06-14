import { useState, type FormEvent } from 'react';

interface LoginPageProps {
  onLogin: (username: string, password: string) => void;
  setupRequired: boolean;
}

export function LoginPage({ onLogin, setupRequired }: LoginPageProps) {
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [confirm, setConfirm] = useState('');
  const [isSetup, setIsSetup] = useState(setupRequired);

  const handleSubmit = (e: FormEvent) => {
    e.preventDefault();
    if (isSetup && password !== confirm) {
      alert('Passwords do not match');
      return;
    }
    onLogin(username, password);
  };

  return (
    <div style={{
      minHeight: '100vh',
      display: 'flex',
      alignItems: 'center',
      justifyContent: 'center',
      background: 'var(--bg-secondary, #f8fafc)',
    }}>
      <div style={{
        width: 360,
        padding: 32,
        background: 'var(--bg-primary, #fff)',
        borderRadius: 12,
        boxShadow: '0 4px 24px rgba(0,0,0,0.06)',
        border: '1px solid var(--border, #e2e8f0)',
      }}>
        <div style={{ textAlign: 'center', marginBottom: 24 }}>
          <div style={{
            width: 48, height: 48, borderRadius: 10,
            background: 'linear-gradient(135deg, var(--accent, #6366F1), #8B5CF6)',
            display: 'flex', alignItems: 'center', justifyContent: 'center',
            margin: '0 auto 12px', fontSize: 22, fontWeight: 700, color: '#fff',
          }}>M</div>
          <h2 style={{ margin: 0, fontSize: 20 }}>AginxBrain</h2>
          <p style={{ margin: '6px 0 0', fontSize: 13, color: 'var(--text-muted, #64748b)' }}>
            {isSetup ? 'Create admin account' : 'Admin sign in'}
          </p>
        </div>

        <form onSubmit={handleSubmit} style={{ display: 'flex', flexDirection: 'column', gap: 14 }}>
          <label style={{ fontSize: 13, color: 'var(--text-secondary, #475569)' }}>
            Username
            <input
              type="text"
              value={username}
              onChange={(e) => setUsername(e.target.value)}
              required
              style={{
                width: '100%', marginTop: 6, padding: '10px 12px', fontSize: 14,
                border: '1px solid var(--border, #e2e8f0)', borderRadius: 8,
                background: 'var(--bg-primary, #fff)', color: 'var(--text-primary, #0f172a)',
                boxSizing: 'border-box',
              }}
            />
          </label>
          <label style={{ fontSize: 13, color: 'var(--text-secondary, #475569)' }}>
            Password
            <input
              type="password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              required
              minLength={6}
              style={{
                width: '100%', marginTop: 6, padding: '10px 12px', fontSize: 14,
                border: '1px solid var(--border, #e2e8f0)', borderRadius: 8,
                background: 'var(--bg-primary, #fff)', color: 'var(--text-primary, #0f172a)',
                boxSizing: 'border-box',
              }}
            />
          </label>
          {isSetup && (
            <label style={{ fontSize: 13, color: 'var(--text-secondary, #475569)' }}>
              Confirm password
              <input
                type="password"
                value={confirm}
                onChange={(e) => setConfirm(e.target.value)}
                required
                style={{
                  width: '100%', marginTop: 6, padding: '10px 12px', fontSize: 14,
                  border: '1px solid var(--border, #e2e8f0)', borderRadius: 8,
                  background: 'var(--bg-primary, #fff)', color: 'var(--text-primary, #0f172a)',
                  boxSizing: 'border-box',
                }}
              />
            </label>
          )}
          <button
            type="submit"
            style={{
              marginTop: 6, padding: '10px 16px', fontSize: 14, fontWeight: 600,
              background: 'var(--accent, #6366F1)', color: '#fff', border: 'none',
              borderRadius: 8, cursor: 'pointer',
            }}
          >
            {isSetup ? 'Create account' : 'Sign in'}
          </button>
        </form>

        {!setupRequired && (
          <button
            onClick={() => setIsSetup(!isSetup)}
            style={{
              marginTop: 16, background: 'transparent', border: 'none',
              color: 'var(--text-muted, #64748b)', fontSize: 12, cursor: 'pointer',
              width: '100%',
            }}
          >
            {isSetup ? 'Already have an account? Sign in' : 'Need to create admin? Setup'}
          </button>
        )}
      </div>
    </div>
  );
}
