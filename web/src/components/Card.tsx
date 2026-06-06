import React from 'react';

export function Card({ children, style, hoverable, ...props }: {
  children: React.ReactNode;
  style?: React.CSSProperties;
  hoverable?: boolean;
} & React.HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      style={{
        padding: 16,
        background: 'var(--bg-card)',
        border: '1px solid var(--border)',
        borderRadius: 'var(--radius-md)',
        boxShadow: 'var(--shadow)',
        transition: 'var(--transition)',
        ...style,
      }}
      {...props}
    >
      {children}
    </div>
  );
}
