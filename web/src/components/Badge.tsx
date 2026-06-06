import React from 'react';

export function Badge({ children, color, style }: {
  children: React.ReactNode;
  color?: string;
  style?: React.CSSProperties;
}) {
  return (
    <span style={{
      display: 'inline-flex', alignItems: 'center',
      padding: '2px 10px', borderRadius: 'var(--radius-full)', fontSize: 12,
      fontWeight: 500, lineHeight: '20px',
      background: color || 'var(--text-muted)', color: '#fff',
      ...style,
    }}>
      {children}
    </span>
  );
}
