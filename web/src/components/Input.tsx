import React from 'react';

export function Input({ label, style, ...props }: {
  label?: string;
  style?: React.CSSProperties;
} & React.InputHTMLAttributes<HTMLInputElement>) {
  return (
    <div>
      {label && <label style={{ display: 'block', fontSize: 12, color: 'var(--text-secondary)', marginBottom: 4, fontWeight: 500 }}>{label}</label>}
      <input
        style={{
          width: '100%', padding: '8px 12px',
          background: 'var(--bg-input)', border: '1px solid var(--border)',
          borderRadius: 'var(--radius-sm)', color: 'var(--text-primary)',
          outline: 'none', transition: 'border-color var(--transition)',
          ...style,
        }}
        onFocus={e => { e.currentTarget.style.borderColor = 'var(--border-focus)'; }}
        onBlur={e => { e.currentTarget.style.borderColor = 'var(--border)'; }}
        {...props}
      />
    </div>
  );
}

export function Select({ label, style, children, ...props }: {
  label?: string;
  style?: React.CSSProperties;
  children: React.ReactNode;
} & React.SelectHTMLAttributes<HTMLSelectElement>) {
  return (
    <div>
      {label && <label style={{ display: 'block', fontSize: 12, color: 'var(--text-secondary)', marginBottom: 4, fontWeight: 500 }}>{label}</label>}
      <select
        style={{
          width: '100%', padding: '8px 12px',
          background: 'var(--bg-input)', border: '1px solid var(--border)',
          borderRadius: 'var(--radius-sm)', color: 'var(--text-primary)',
          outline: 'none', cursor: 'pointer',
          ...style,
        }}
        {...props}
      >
        {children}
      </select>
    </div>
  );
}
