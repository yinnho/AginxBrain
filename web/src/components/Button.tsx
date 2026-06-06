import React from 'react';

type Variant = 'primary' | 'secondary' | 'danger' | 'ghost' | 'success';

const styles: Record<Variant, React.CSSProperties> = {
  primary: {
    padding: '7px 16px', borderRadius: 'var(--radius-sm)', border: 'none',
    background: 'var(--accent)', color: '#fff', cursor: 'pointer', fontWeight: 500,
  },
  secondary: {
    padding: '7px 16px', borderRadius: 'var(--radius-sm)', border: '1px solid var(--border)',
    background: 'transparent', color: 'var(--text-secondary)', cursor: 'pointer',
  },
  danger: {
    padding: '7px 16px', borderRadius: 'var(--radius-sm)', border: 'none',
    background: 'var(--danger-dim)', color: 'var(--danger)', cursor: 'pointer',
  },
  ghost: {
    padding: '7px 16px', borderRadius: 'var(--radius-sm)', border: 'none',
    background: 'transparent', color: 'var(--text-secondary)', cursor: 'pointer',
  },
  success: {
    padding: '7px 16px', borderRadius: 'var(--radius-sm)', border: 'none',
    background: 'var(--success)', color: '#fff', cursor: 'pointer', fontWeight: 500,
  },
};

export function Button({ variant = 'secondary', disabled, style, children, ...props }: {
  variant?: Variant;
  disabled?: boolean;
  style?: React.CSSProperties;
  children: React.ReactNode;
} & React.ButtonHTMLAttributes<HTMLButtonElement>) {
  return (
    <button
      disabled={disabled}
      style={{
        ...styles[variant],
        opacity: disabled ? 0.4 : 1,
        cursor: disabled ? 'not-allowed' : 'pointer',
        fontSize: 13,
        ...style,
      }}
      {...props}
    >
      {children}
    </button>
  );
}
